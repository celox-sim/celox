//! Function call arguments: binding checks, bound formals, materializing inputs and coercing outputs.

use super::*;

impl<'a> FfParser<'a> {
    /// Read a runtime-indexed select of a formal bound to an expression:
    /// evaluate the whole actual, then select from that value.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn load_dynamic_bound_function_access<A>(
        &mut self,
        var_id: VarId,
        bound_expr: &Expression,
        index: &VarIndex,
        select: &VarSelect,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let formal_width = resolve_total_width(self.module, &self.module.variables[&var_id])?;
        self.materialize_bound_function_access(
            var_id,
            bound_expr,
            BitAccess::new(0, formal_width - 1),
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let whole = self.stack.pop_back().expect("bound formal value");
        let selected = self.emit_register_select(
            whole, var_id, index, select, domain, convert, sources, ir_builder,
        )?;
        self.stack.push_back(selected);
        Ok(())
    }

    pub(super) fn materialize_bound_function_access<A>(
        &mut self,
        var_id: VarId,
        bound_expr: &Expression,
        access: BitAccess,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let formal_width = self
            .module
            .variables
            .get(&var_id)
            .map(|var| resolve_total_width(self.module, var))
            .transpose()?;
        self.parse_expression(
            bound_expr,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            formal_width,
        )?;
        let bound_reg = self.stack.pop_back().unwrap();
        let coerced_reg = if let Some(var) = self.module.variables.get(&var_id) {
            let formal_width = formal_width.expect("formal width must exist for bound argument");
            self.coerce_register_to_formal(
                ir_builder,
                bound_reg,
                formal_width,
                bound_expr.comptime().r#type.signed,
                var.r#type.signed,
                var.r#type.is_2state(),
            )
        } else {
            bound_reg
        };
        let sliced = self.emit_register_slice(coerced_reg, access, ir_builder);
        self.stack.push_back(sliced);
        Ok(())
    }

    pub(super) fn coerce_function_output_to_actual<A>(
        &self,
        reg: RegisterId,
        formal_id: VarId,
        dsts: &[AssignDestination],
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        if let [destination] = dsts {
            if let Some(op) = crate::float::conversion(
                &self.module.variables[&formal_id].r#type,
                &destination.comptime.r#type,
            ) {
                let result = ir_builder.alloc_bit(
                    destination.comptime.r#type.total_width().unwrap(),
                    destination.comptime.r#type.signed,
                );
                ir_builder.emit(SIRInstruction::Unary(result, op, reg));
                return Ok(result);
            }
        }
        let width = dsts
            .iter()
            .map(|dst| get_access_width(self.module, dst.id, &dst.index, &dst.select))
            .sum::<Result<usize, ParserError>>()?;
        let signed = self.module.variables[&formal_id].r#type.signed;
        if width < ir_builder.register(&reg).width() {
            let truncated = match ir_builder.register(&reg) {
                RegisterType::Logic { .. } => ir_builder.alloc_logic(width),
                RegisterType::Bit { .. } => ir_builder.alloc_bit(width, signed),
            };
            // Copying a narrower value preserves Z; a bitwise mask would
            // turn it into X even in the retained bits.
            ir_builder.emit(SIRInstruction::Slice(truncated, reg, 0, width));
            return Ok(truncated);
        }
        // Resize the complete formal value before splitting concatenated
        // destinations. Logic registers do not carry signedness themselves.
        Ok(self.cast_reg_width_ext(ir_builder, reg, width, signed))
    }

    fn default_expr_matches_formal(expr: &Expression, formal_shape: &[usize]) -> bool {
        Self::expr_shape_matches_formal(expr, formal_shape)
            || (!formal_shape.is_empty() && expr.comptime().r#type.array.is_empty())
    }

    fn expr_shape_matches_formal(expr: &Expression, formal_shape: &[usize]) -> bool {
        match expr {
            Expression::Term(factor)
                if matches!(factor.as_ref(), Factor::Anonymous(_) | Factor::Unknown(_)) =>
            {
                false
            }
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

    pub(super) fn validate_function_call_bindings(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        function_body: &veryl_analyzer::ir::FunctionBody,
    ) -> Result<(), ParserError> {
        for (arg_path, arg_id) in &function_body.arg_map {
            let Some(arg_expr) = function_call_arg(&call.inputs, arg_path) else {
                continue;
            };
            let formal = &self.module.variables[arg_id];
            if !self.actual_matches_formal_shape(formal, arg_expr) {
                return Err(ParserError::illegal_context(
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

    #[allow(clippy::too_many_arguments)]
    fn materialize_array_actual_items<A>(
        &mut self,
        expr: &Expression,
        force_materialization: bool,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        // Array views deliberately defer pure elements until the callee reads
        // them. An unused formal therefore never creates a view, but its
        // effectful actual elements must still run at the call site. A pure
        // item before an effectful one must also be snapshotted before that
        // effect changes its dependencies. Preserve every materialized value
        // under its token range so a later view access reuses it instead of
        // replaying the effect or observing a newer value.
        if let Expression::ArrayLiteral(items, _) = expr {
            let last_effectful_item = items.iter().rposition(|item| {
                let item_expr = match item {
                    ArrayLiteralItem::Value(expr, _) | ArrayLiteralItem::Defaul(expr) => expr,
                };
                self.expression_needs_eager_evaluation(item_expr)
            });
            for (item_index, item) in items.iter().enumerate() {
                let item_expr = match item {
                    ArrayLiteralItem::Value(expr, _) | ArrayLiteralItem::Defaul(expr) => expr,
                };
                self.materialize_array_actual_items(
                    item_expr,
                    force_materialization
                        || last_effectful_item.is_some_and(|last| item_index <= last),
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
            }
            return Ok(());
        }

        if !force_materialization
            && !self.expression_needs_eager_evaluation(expr)
            && !Self::expression_contains_function_call(expr)
        {
            return Ok(());
        }

        self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
        let value = self
            .stack
            .pop_back()
            .expect("Array literal item materialization failed");
        self.function_expression_value_stack
            .last_mut()
            .expect("Function expression value scope is active")
            .insert(expr.token_range(), value);
        Ok(())
    }

    /// Whether the unpacked-array actual at `arg_index` could observe a write
    /// made later in the call (by a later actual, an output copy-out, or the
    /// callee). The symbolic lowering reads array actuals lazily through
    /// views, which cannot represent such a call.
    pub(super) fn unpacked_input_aliases_later_effect(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        function_body: &veryl_analyzer::ir::FunctionBody,
        ordered_arg_paths: &[veryl_analyzer::ir::VarPath],
        arg_index: usize,
    ) -> bool {
        let Some(actual) = function_call_arg(&call.inputs, &ordered_arg_paths[arg_index]) else {
            return false;
        };
        let output_ids: HashSet<VarId> = call
            .outputs
            .values()
            .flat_map(|destinations| destinations.iter().map(|dst| dst.id))
            .collect();
        let mut dependencies = HashSet::default();
        self.collect_expression_read_variables(actual, &mut dependencies, false);
        let callee_writes_dependency = self.statements_write_any(
            &function_body.statements,
            &dependencies,
            &mut HashSet::default(),
        );
        let mut array_variables = HashSet::default();
        self.collect_expression_read_variables(actual, &mut array_variables, true);
        let aliases_later_write = ordered_arg_paths
            .iter()
            .skip(arg_index + 1)
            .filter_map(|path| function_call_arg(&call.inputs, path))
            .any(|expr| self.expression_writes_any(expr, &array_variables));
        let aliases_output_write = !dependencies.is_disjoint(&output_ids)
            || call
                .outputs
                .values()
                .flatten()
                .any(|dst| self.assignment_destination_writes_any(dst, &dependencies));
        let callee_writes_array = self.statements_write_any(
            &function_body.statements,
            &array_variables,
            &mut HashSet::default(),
        );
        let cannot_snapshot_before_callee_write =
            callee_writes_dependency && !matches!(actual, Expression::ArrayLiteral(_, _));
        aliases_later_write
            || aliases_output_write
            || callee_writes_array
            || cannot_snapshot_before_callee_write
    }

    /// Whether any unpacked-array actual of `call` aliases a later effect.
    pub(super) fn call_has_unpacked_input_alias(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        function_body: &veryl_analyzer::ir::FunctionBody,
        ordered_arg_paths: &[veryl_analyzer::ir::VarPath],
    ) -> bool {
        ordered_arg_paths
            .iter()
            .enumerate()
            .any(|(arg_index, arg_path)| {
                function_body.arg_map.get(arg_path).is_some_and(|arg_id| {
                    !self.module.variables[arg_id].r#type.array.is_empty()
                        && self.unpacked_input_aliases_later_effect(
                            call,
                            function_body,
                            ordered_arg_paths,
                            arg_index,
                        )
                })
            })
    }

    pub(super) fn materialize_function_inputs<A>(
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
        let has_effectful_output_destination = call
            .outputs
            .values()
            .flatten()
            .any(|dst| self.assignment_destination_needs_eager_evaluation(dst));
        let last_effectful_arg = ordered_arg_paths.iter().rposition(|arg_path| {
            function_call_arg(&call.inputs, arg_path).is_some_and(|actual| {
                super::expression::expression_has_side_effect(actual)
                    || self.expression_has_runtime_effect(actual)
            })
        });
        for (arg_index, arg_path) in ordered_arg_paths.iter().enumerate() {
            let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                continue;
            };
            let Some(actual) = function_call_arg(&call.inputs, arg_path) else {
                continue;
            };
            let must_snapshot_for_order = last_effectful_arg.is_some_and(|last| arg_index <= last);
            let formal = &self.module.variables[arg_id];
            let mut dependencies = HashSet::default();
            self.collect_expression_read_variables(actual, &mut dependencies, false);
            let callee_writes_dependency = self.statements_write_any(
                &function_body.statements,
                &dependencies,
                &mut HashSet::default(),
            );
            if !formal.r#type.array.is_empty() {
                if self.unpacked_input_aliases_later_effect(
                    call,
                    function_body,
                    ordered_arg_paths,
                    arg_index,
                ) {
                    // Such calls are lowered inline, which snapshots inputs.
                    return Err(ParserError::internal(
                        "unpacked function argument aliases later effect",
                        format!("{actual}"),
                        Some(&call.comptime.token),
                    ));
                }
                self.materialize_array_actual_items(
                    actual,
                    (must_snapshot_for_order || callee_writes_dependency)
                        && matches!(actual, Expression::ArrayLiteral(_, _)),
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                // Array arguments are represented by call-scoped views. The
                // view snapshots each observed element in source order while
                // leaving unobserved, side-effect-free elements lazy; a scalar
                // register cannot preserve that unpacked behavior.
                continue;
            }
            if !must_snapshot_for_order
                && !has_effectful_output_destination
                && !Self::expression_references_any(actual, &output_ids)
                && !callee_writes_dependency
                && !Self::expression_contains_function_call(actual)
            {
                continue;
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

    pub(super) fn get_bound_function_array_view(&self, var_id: VarId) -> Option<VarId> {
        for frame in (0..self.function_arg_stack.len()).rev() {
            if self.function_array_view_enabled_stack[frame]
                && self.function_arg_stack[frame].contains_key(&var_id)
            {
                return self.function_array_view_stack[frame]
                    .get(&var_id)
                    .filter(|view| view.initialized.is_none())
                    .map(|view| view.backing_var_id);
            }
        }
        None
    }
}
