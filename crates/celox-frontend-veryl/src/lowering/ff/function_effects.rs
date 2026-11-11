//! Runtime effects of symbolically lowered function bodies: nonlocal writes, system tasks, dynamic copy-outs.

use super::*;

impl<'a> FfParser<'a> {
    fn assignment_destination_needs_direct_runtime_copyout(&self, dst: &AssignDestination) -> bool {
        self.module.variables[&dst.id].affiliation != Affiliation::Function
            && !is_static_access(&dst.index, &dst.select)
    }

    pub(super) fn retain_direct_runtime_copyouts(
        &self,
        call: &mut veryl_analyzer::ir::FunctionCall,
        retain_direct: bool,
    ) {
        // Dynamic nonlocal destinations cannot be represented by the
        // whole-variable symbolic state. Leave them on the runtime call so
        // normal store lowering computes their offsets. CallArgs preserves
        // argument positions, so filtered entries remain as empty vectors and
        // output consumers skip them.
        for dsts in call.outputs.values_mut() {
            dsts.retain(|dst| {
                self.assignment_destination_needs_direct_runtime_copyout(dst) == retain_direct
            });
        }
    }

    pub(super) fn function_call_emits_nonlocal_runtime_write(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
    ) -> bool {
        if call
            .outputs
            .values()
            .flatten()
            .any(|dst| self.assignment_destination_needs_direct_runtime_copyout(dst))
        {
            return true;
        }

        let nonlocal_ids: HashSet<_> = self
            .module
            .variables
            .iter()
            .filter_map(|(id, variable)| {
                (variable.affiliation != Affiliation::Function).then_some(*id)
            })
            .collect();
        self.module
            .functions
            .get(&call.id)
            .and_then(|function| {
                if let Some(index) = &call.index {
                    function.get_function(index)
                } else {
                    function.get_function(&[])
                }
            })
            .is_some_and(|body| {
                self.statements_write_any(&body.statements, &nonlocal_ids, &mut HashSet::default())
            })
    }

    pub(super) fn mark_emitted_call_state(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        state: &mut HashMap<VarId, Expression>,
    ) {
        // The retained runtime call has already performed its body-side
        // nonlocal writes and dynamic copy-outs. Preserve their expressions
        // for subsequent symbolic reads, but mark them so the enclosing call
        // does not store them again. Nonlocal outputs filtered out of the
        // runtime call still require an ordinary symbolic copy-out.
        let symbolic_nonlocal_outputs: HashSet<_> = call
            .outputs
            .values()
            .flatten()
            .filter_map(|dst| {
                (self.module.variables[&dst.id].affiliation != Affiliation::Function)
                    .then_some(dst.id)
            })
            .collect();
        for (id, expr) in state {
            if self.module.variables[id].affiliation != Affiliation::Function
                && !symbolic_nonlocal_outputs.contains(id)
            {
                Self::mark_emitted_function_state(expr);
            }
        }
    }

    fn is_whole_variable_reference(expr: &Expression, var_id: VarId) -> bool {
        matches!(
            expr,
            Expression::Term(factor)
                if matches!(
                    factor.as_ref(),
                    Factor::Variable(id, index, select, _)
                        if *id == var_id
                            && index.0.is_empty()
                            && select.0.is_empty()
                            && select.1.is_none()
                )
        )
    }

    fn collect_guarded_nonlocal_writes(
        var_id: VarId,
        expr: &Expression,
        active: FunctionPathCondition,
        writes: &mut Vec<(VarId, Expression, FunctionPathCondition)>,
    ) {
        if matches!(active, FunctionPathCondition::Never)
            || Self::is_whole_variable_reference(expr, var_id)
            || Self::is_emitted_function_state(expr)
        {
            return;
        }
        if let Expression::Ternary(condition, then_expr, else_expr, comptime) = expr
            && Self::is_function_state_merge(comptime)
        {
            Self::collect_guarded_nonlocal_writes(
                var_id,
                then_expr,
                Self::function_path_and(active.clone(), condition.as_ref().clone()),
                writes,
            );
            Self::collect_guarded_nonlocal_writes(
                var_id,
                else_expr,
                Self::function_path_and_not(active, condition.as_ref().clone()),
                writes,
            );
        } else {
            writes.push((var_id, expr.clone(), active));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_nonlocal_function_state_writes<A>(
        &mut self,
        state: &HashMap<VarId, Expression>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        // Function-local state is consumed while lowering the enclosing call.
        // Any other entry was introduced by a nested output actual (or a
        // direct write in the function body) and needs an actual store in the
        // caller's FF region.
        let mut symbolic_writes = Vec::new();
        for (&var_id, expr) in state {
            let variable = &self.module.variables[&var_id];
            if variable.affiliation == Affiliation::Function {
                continue;
            }
            Self::collect_guarded_nonlocal_writes(
                var_id,
                expr,
                FunctionPathCondition::Always,
                &mut symbolic_writes,
            );
        }

        let mut writes = Vec::new();
        for (var_id, expr, active) in symbolic_writes {
            let variable = &self.module.variables[&var_id];
            let dst = AssignDestination {
                id: var_id,
                path: variable.path.clone(),
                index: VarIndex::default(),
                select: VarSelect::default(),
                comptime: Comptime {
                    r#type: variable.r#type.clone(),
                    ..Default::default()
                },
                token: TokenRange::default(),
            };
            let guard = if let FunctionPathCondition::Conditional(condition) = active {
                self.parse_expression(
                    &condition, targets, domain, convert, sources, ir_builder, None,
                )?;
                let condition = self
                    .stack
                    .pop_back()
                    .expect("Nonlocal function state guard evaluation failed");
                Some(self.lower_procedural_condition(condition, ir_builder))
            } else {
                None
            };
            self.parse_expression(&expr, targets, domain, convert, sources, ir_builder, None)?;
            let value = self
                .stack
                .pop_back()
                .expect("Nonlocal function state evaluation failed");
            let value = self.coerce_register_to_variable_type(
                value,
                var_id,
                expression_signed(&expr),
                ir_builder,
            )?;
            writes.push((dst, value, guard));
        }

        // Resolve every RHS against the caller's pre-copy-out state before
        // mutating any nonlocal destination. `state` is a hash map, so
        // interleaving evaluation and stores would make the result depend on
        // its iteration order when one final expression reads another target.
        for (dst, value, guard) in writes {
            let merge_block = if let Some(guard) = guard {
                let write_block = ir_builder.new_block();
                let merge_block = ir_builder.new_block();
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: guard,
                    true_block: (write_block, vec![]),
                    false_block: (merge_block, vec![]),
                });
                ir_builder.switch_to_block(write_block);
                Some(merge_block)
            } else {
                None
            };
            self.emit_multi_dst_assign(
                value,
                std::slice::from_ref(&dst),
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            if let Some(merge_block) = merge_block {
                ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
                ir_builder.switch_to_block(merge_block);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn flush_captured_nonlocal_state_before_call<A>(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        if !self.function_call_emits_nonlocal_runtime_write(call) {
            return Ok(());
        }
        let Some(state) = self
            .get_bound_function_event_arg_state(call.comptime.token)
            .cloned()
        else {
            return Ok(());
        };
        if !state
            .keys()
            .any(|id| self.module.variables[id].affiliation != Affiliation::Function)
        {
            return Ok(());
        }
        self.emit_nonlocal_function_state_writes(
            &state, targets, domain, convert, sources, ir_builder,
        )
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
                self.frames.function_arg_stack.push(state.clone());
                self.frames
                    .function_array_view_stack
                    .push(HashMap::default());
                self.frames.function_array_view_enabled_stack.push(false);
                self.frames
                    .function_event_arg_state_stack
                    .push(expression_states.clone());
                let result = self
                    .parse_expression(&expr, targets, domain, convert, sources, ir_builder, None);
                self.frames.function_event_arg_state_stack.pop();
                self.frames.function_array_view_enabled_stack.pop();
                self.frames.function_array_view_stack.pop();
                self.frames.function_arg_stack.pop();
                result?;
                self.stack
                    .pop_back()
                    .expect("Function runtime expression evaluation failed")
            }
            FunctionPathCondition::Conditional(condition) => {
                let pre_defined = self.defined.defined_ranges.clone();
                let pre_dynamic = self.defined.dynamic_defined_vars.clone();
                *state = self.merge_expression_states(
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
                let signed = expression_signed(&expr);
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
                self.frames.function_arg_stack.push(evaluated_state.clone());
                self.frames
                    .function_array_view_stack
                    .push(HashMap::default());
                self.frames.function_array_view_enabled_stack.push(false);
                self.frames
                    .function_event_arg_state_stack
                    .push(expression_states.clone());
                let parse_result = self
                    .parse_expression(&expr, targets, domain, convert, sources, ir_builder, None);
                self.frames.function_event_arg_state_stack.pop();
                self.frames.function_array_view_enabled_stack.pop();
                self.frames.function_array_view_stack.pop();
                self.frames.function_arg_stack.pop();
                parse_result?;
                let effect_value = self
                    .stack
                    .pop_back()
                    .expect("Guarded function runtime expression evaluation failed");
                let effect_value =
                    self.coerce_register_to_type(effect_value, &result_type, signed, ir_builder);
                let effect_defined =
                    std::mem::replace(&mut self.defined.defined_ranges, pre_defined.clone());
                let effect_dynamic =
                    std::mem::replace(&mut self.defined.dynamic_defined_vars, pre_dynamic.clone());
                ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![effect_value]));

                ir_builder.switch_to_block(skip_block);
                let dummy = ir_builder.alloc_reg(result_type);
                ir_builder.emit(SIRInstruction::Imm(dummy, SIRValue::new(0u8)));
                ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![dummy]));
                ir_builder.switch_to_block(merge_block);
                self.defined.defined_ranges =
                    self.intersect_defined_states(pre_defined, effect_defined);
                self.defined.dynamic_defined_vars =
                    self.intersect_dynamic_vars(pre_dynamic, effect_dynamic);
                result
            }
        };
        self.frames
            .function_expression_value_stack
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
        *state = self.apply_state_transition_on_path(active, &guard_state, next_state);
        let pre_defined = self.defined.defined_ranges.clone();
        let pre_dynamic = self.defined.dynamic_defined_vars.clone();
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

        self.frames.function_event_arg_state_stack.push(arg_states);
        let result =
            self.parse_system_task_statement(&call, targets, domain, convert, sources, ir_builder);
        self.frames.function_event_arg_state_stack.pop();
        result?;

        if let Some(merge_block) = merge_block {
            let effect_defined =
                std::mem::replace(&mut self.defined.defined_ranges, pre_defined.clone());
            let effect_dynamic =
                std::mem::replace(&mut self.defined.dynamic_defined_vars, pre_dynamic.clone());
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
            ir_builder.switch_to_block(merge_block);
            self.defined.defined_ranges =
                self.intersect_defined_states(pre_defined, effect_defined);
            self.defined.dynamic_defined_vars =
                self.intersect_dynamic_vars(pre_dynamic, effect_dynamic);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_dynamic_nonlocal_function_assignment<A>(
        &mut self,
        assign: &AssignStatement,
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

        let mut assign = assign.clone();
        let guard_state = state.clone();
        if state
            .iter()
            .any(|(id, _)| self.module.variables[id].affiliation != Affiliation::Function)
        {
            let flushed_state = state.clone();
            self.emit_nonlocal_function_state_writes(
                &flushed_state,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            assign.expr = self.substitute_function_expr(&assign.expr, &flushed_state);
            assign.dst = assign
                .dst
                .iter()
                .map(|dst| self.substitute_assignment_destination(dst, &flushed_state))
                .collect();
            state.retain(|id, _| self.module.variables[id].affiliation == Affiliation::Function);
        }
        if self.expression_needs_runtime_materialization(&assign.expr) {
            assign.expr = self.materialize_function_runtime_expression(
                &assign.expr,
                state,
                active,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
        }

        let pre_defined = self.defined.defined_ranges.clone();
        let pre_dynamic = self.defined.dynamic_defined_vars.clone();
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
            let store_block = ir_builder.new_block();
            let merge_block = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: condition,
                true_block: (store_block, vec![]),
                false_block: (merge_block, vec![]),
            });
            ir_builder.switch_to_block(store_block);
            Some(merge_block)
        } else {
            None
        };

        self.frames.function_arg_stack.push(state.clone());
        self.frames
            .function_array_view_stack
            .push(HashMap::default());
        self.frames.function_array_view_enabled_stack.push(false);
        let result =
            self.parse_assign_statement(&assign, targets, domain, convert, sources, ir_builder);
        self.frames.function_array_view_enabled_stack.pop();
        self.frames.function_array_view_stack.pop();
        self.frames.function_arg_stack.pop();
        result?;

        if let Some(merge_block) = merge_block {
            let store_defined =
                std::mem::replace(&mut self.defined.defined_ranges, pre_defined.clone());
            let store_dynamic =
                std::mem::replace(&mut self.defined.dynamic_defined_vars, pre_dynamic.clone());
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
            ir_builder.switch_to_block(merge_block);
            self.defined.defined_ranges = self.intersect_defined_states(pre_defined, store_defined);
            self.defined.dynamic_defined_vars =
                self.intersect_dynamic_vars(pre_dynamic, store_dynamic);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_dynamic_nonlocal_statement_call<A>(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
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

        let mut call = call.clone();
        let guard_state = state.clone();
        if state
            .iter()
            .any(|(id, _)| self.module.variables[id].affiliation != Affiliation::Function)
        {
            let flushed_state = state.clone();
            self.emit_nonlocal_function_state_writes(
                &flushed_state,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            for input in call.inputs.values_mut() {
                *input = self.substitute_function_expr(input, &flushed_state);
            }
            for dsts in call.outputs.values_mut() {
                *dsts = dsts
                    .iter()
                    .map(|dst| self.substitute_assignment_destination(dst, &flushed_state))
                    .collect();
            }
            state.retain(|id, _| self.module.variables[id].affiliation == Affiliation::Function);
        }

        let pre_defined = self.defined.defined_ranges.clone();
        let pre_dynamic = self.defined.dynamic_defined_vars.clone();
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
            let call_block = ir_builder.new_block();
            let merge_block = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: condition,
                true_block: (call_block, vec![]),
                false_block: (merge_block, vec![]),
            });
            ir_builder.switch_to_block(call_block);
            Some(merge_block)
        } else {
            None
        };

        self.frames.function_arg_stack.push(state.clone());
        self.frames
            .function_array_view_stack
            .push(HashMap::default());
        self.frames.function_array_view_enabled_stack.push(false);
        let result = self
            .parse_function_call_statement(&call, targets, domain, convert, sources, ir_builder);
        self.frames.function_array_view_enabled_stack.pop();
        self.frames.function_array_view_stack.pop();
        self.frames.function_arg_stack.pop();
        result?;

        if let Some(merge_block) = merge_block {
            let call_defined =
                std::mem::replace(&mut self.defined.defined_ranges, pre_defined.clone());
            let call_dynamic =
                std::mem::replace(&mut self.defined.dynamic_defined_vars, pre_dynamic.clone());
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
            ir_builder.switch_to_block(merge_block);
            self.defined.defined_ranges = self.intersect_defined_states(pre_defined, call_defined);
            self.defined.dynamic_defined_vars =
                self.intersect_dynamic_vars(pre_dynamic, call_dynamic);
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
                    let direct_dynamic_nonlocal = assign.dst.len() == 1
                        && self.assignment_destination_needs_direct_runtime_copyout(&assign.dst[0]);
                    if direct_dynamic_nonlocal
                        && !self.assignment_destination_needs_eager_evaluation(&assign.dst[0])
                    {
                        self.emit_dynamic_nonlocal_function_assignment(
                            assign, &mut state, &active, targets, domain, convert, sources,
                            ir_builder,
                        )?;
                        continue;
                    }
                    if assign
                        .dst
                        .iter()
                        .any(|dst| self.assignment_destination_needs_eager_evaluation(dst))
                    {
                        return Err(ParserError::internal(
                            "effectful assignment destination in function body (calls needing it are lowered inline)",
                            format!("{statement}"),
                            Some(&assign.token),
                        ));
                    }
                    let materialized_rhs =
                        if self.expression_needs_runtime_materialization(&assign.expr) {
                            Some(self.materialize_function_runtime_expression(
                                &assign.expr,
                                &mut state,
                                &active,
                                targets,
                                domain,
                                convert,
                                sources,
                                ir_builder,
                            )?)
                        } else {
                            None
                        };
                    let base = state.clone();
                    let transitioned = if let Some(rhs) = materialized_rhs.as_ref() {
                        self.apply_assignment_to_function_state(assign, rhs, &base)?
                    } else {
                        self.apply_statement_to_function_state(statement, &base)?
                    };
                    state = self.apply_state_transition_on_path(&active, &base, transitioned);
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
                    let condition =
                        if self.expression_needs_runtime_materialization(&statement.cond) {
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
                            self.substitute_function_expr(&statement.cond, &state)
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
                        self.merge_expression_states(&condition, &base, &then_state, &else_state);
                    active = Self::function_path_or(then_active, else_active);
                }
                Statement::Case(statement) => {
                    let pattern_needs_eager_evaluation = statement.arms.iter().any(|arm| {
                        arm.patterns.iter().any(|pattern| match pattern {
                            CasePattern::Eq(expr) => self.expression_needs_eager_evaluation(expr),
                            CasePattern::Range { lo, hi, .. } => {
                                self.expression_needs_eager_evaluation(lo)
                                    || self.expression_needs_eager_evaluation(hi)
                            }
                        })
                    });
                    if pattern_needs_eager_evaluation
                        || self.expression_needs_runtime_materialization(&statement.case_target)
                    {
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
                    let case_base = state.clone();
                    let mut remaining = active.clone();
                    let mut live_paths = FunctionPathCondition::Never;
                    let mut arm_states = Vec::with_capacity(statement.arms.len());
                    for arm in &statement.arms {
                        let last_effectful_pattern =
                            arm.patterns.iter().rposition(|pattern| match pattern {
                                CasePattern::Eq(expr) => {
                                    self.expression_needs_eager_evaluation(expr)
                                }
                                CasePattern::Range { lo, hi, .. } => {
                                    self.expression_needs_eager_evaluation(lo)
                                        || self.expression_needs_eager_evaluation(hi)
                                }
                            });
                        let mut pattern_remaining = remaining.clone();
                        let mut arm_active = FunctionPathCondition::Never;
                        let mut arm_condition = None;
                        for (pattern_index, pattern) in arm.patterns.iter().enumerate() {
                            let snapshot_for_later_effect =
                                last_effectful_pattern.is_some_and(|last| pattern_index < last);
                            match pattern {
                                CasePattern::Eq(expr) => {
                                    if snapshot_for_later_effect
                                        || self.expression_needs_runtime_materialization(expr)
                                    {
                                        self.materialize_function_runtime_expression(
                                            expr,
                                            &mut state,
                                            &pattern_remaining,
                                            targets,
                                            domain,
                                            convert,
                                            sources,
                                            ir_builder,
                                        )?;
                                    }
                                }
                                CasePattern::Range { lo, hi, .. } => {
                                    let hi_is_effectful =
                                        self.expression_needs_eager_evaluation(hi);
                                    if snapshot_for_later_effect
                                        || hi_is_effectful
                                        || self.expression_needs_runtime_materialization(lo)
                                    {
                                        self.materialize_function_runtime_expression(
                                            lo,
                                            &mut state,
                                            &pattern_remaining,
                                            targets,
                                            domain,
                                            convert,
                                            sources,
                                            ir_builder,
                                        )?;
                                    }
                                    if snapshot_for_later_effect
                                        || hi_is_effectful
                                        || self.expression_needs_assignment_snapshot(hi)
                                    {
                                        let lower_condition = self.substitute_function_expr(
                                            &Expression::Binary(
                                                lo.clone(),
                                                Op::LessEq,
                                                statement.case_target.clone(),
                                                Box::new(Comptime::create_unknown(
                                                    TokenRange::default(),
                                                )),
                                            ),
                                            &state,
                                        );
                                        let upper_active = Self::function_path_and(
                                            pattern_remaining.clone(),
                                            Self::function_control_may_be_true(lower_condition),
                                        );
                                        self.materialize_function_runtime_expression(
                                            hi,
                                            &mut state,
                                            &upper_active,
                                            targets,
                                            domain,
                                            convert,
                                            sources,
                                            ir_builder,
                                        )?;
                                    }
                                }
                            }
                            let pattern_base = state.clone();
                            let condition = self.substitute_function_expr(
                                &case_arm_condition_expr(
                                    &statement.case_target,
                                    std::slice::from_ref(pattern),
                                ),
                                &pattern_base,
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
                        let (_, arm_live) = self.emit_function_runtime_effects_in_path(
                            &arm.body, ret_id, &base, arm_active, targets, domain, convert,
                            sources, ir_builder,
                        )?;
                        // The emitted state includes the case-selection guard,
                        // which can retain a self-reference for output formals.
                        // Recompute without that guard, but preserve the outer
                        // live-path condition from preceding returns. Case
                        // priority is applied once below.
                        let (arm_state, _) = self.apply_statements_to_function_state_in_path(
                            &arm.body,
                            ret_id,
                            &base,
                            active.clone(),
                        )?;
                        arm_states.push((
                            arm_condition.expect("Case arm must have at least one pattern"),
                            arm_state,
                        ));
                        live_paths = Self::function_path_or(live_paths, arm_live);
                        remaining = pattern_remaining;
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
                    let (default_state, _) = self.apply_statements_to_function_state_in_path(
                        &statement.default,
                        ret_id,
                        &base,
                        active.clone(),
                    )?;
                    state = arm_states.into_iter().rev().fold(
                        default_state,
                        |merged, (condition, arm_state)| {
                            self.merge_expression_states(
                                &condition, &case_base, &arm_state, &merged,
                            )
                        },
                    );
                    active = Self::function_path_or(live_paths, default_live);
                }
                Statement::For(statement) => {
                    return Err(ParserError::internal(
                        "for loop in function body (calls needing it are lowered inline)",
                        "for loop".to_string(),
                        Some(&statement.token),
                    ));
                }
                Statement::FunctionCall(call) => {
                    if call
                        .outputs
                        .values()
                        .flatten()
                        .any(|dst| self.assignment_destination_needs_eager_evaluation(dst))
                    {
                        return Err(ParserError::internal(
                            "effectful function call output destination in function body (calls needing it are lowered inline)",
                            format!("{statement}"),
                            Some(&call.comptime.token),
                        ));
                    }
                    if self.function_call_has_runtime_effect(call, &mut HashSet::default()) {
                        return Err(ParserError::internal(
                            "nested runtime effect in function body (calls needing it are lowered inline)",
                            format!("{statement}"),
                            Some(&call.comptime.token),
                        ));
                    }
                    let Some(function) = self.module.functions.get(&call.id) else {
                        return Err(ParserError::internal(
                            "function call",
                            format!("unknown function id: {:?}", call.id),
                            Some(&call.comptime.token),
                        ));
                    };
                    let ordered_inputs: Vec<_> = function
                        .args
                        .iter()
                        .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path))
                        .filter_map(|path| function_call_arg(&call.inputs, path).cloned())
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
                    let mut state_call = call.clone();
                    self.retain_direct_runtime_copyouts(&mut state_call, false);
                    let mut direct_call = call.clone();
                    self.retain_direct_runtime_copyouts(&mut direct_call, true);
                    let emitted_direct_call =
                        direct_call.outputs.values().any(|dsts| !dsts.is_empty());
                    if emitted_direct_call {
                        self.emit_dynamic_nonlocal_statement_call(
                            &direct_call,
                            &mut state,
                            &active,
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                    }
                    let base = state.clone();
                    let mut transitioned = self.apply_function_call_to_state(&state_call, &base)?;
                    if emitted_direct_call {
                        self.mark_emitted_call_state(&state_call, &mut transitioned);
                    }
                    state = self.apply_state_transition_on_path(&active, &base, transitioned);
                }
                Statement::IfReset(statement) => {
                    return Err(ParserError::illegal_context(
                        "if_reset in function body",
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

    pub(super) fn emit_function_runtime_effects<A>(
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
}
