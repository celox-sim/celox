use super::{Domain, FfParser};
use crate::context_width::{ValueContext, expression_signed};
use crate::{
    HashMap, ParserError,
    bitaccess::{
        celox_value_from_comptime, celox_value_from_comptime_in_context, eval_var_select,
        get_access_width, is_static_access,
    },
    resolve_total_width,
};
use celox_design::{BinaryOp, BitAccess, UnaryOp, VarAtomBase};
use celox_sir::{SIRBuilder, SIRInstruction, SIRValue};
use num_bigint::BigUint;
use num_traits::Zero;

use veryl_analyzer::ir::{ArrayLiteralItem, Expression, Factor, Op, SystemFunctionKind, VarId};

pub(crate) fn expression_has_side_effect(expr: &Expression) -> bool {
    let input_has_side_effect =
        |input: &veryl_analyzer::ir::SystemFunctionInput| expression_has_side_effect(&input.0);
    match expr {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(_, index, select, _) => {
                index.0.iter().any(expression_has_side_effect)
                    || select.0.iter().any(expression_has_side_effect)
                    || select
                        .1
                        .as_ref()
                        .is_some_and(|(_, expr)| expression_has_side_effect(expr))
            }
            Factor::FunctionCall(call) => {
                call.outputs
                    .values()
                    .any(|destinations| !destinations.is_empty())
                    || call.inputs.values().any(expression_has_side_effect)
            }
            Factor::SystemFunctionCall(call) => match &call.kind {
                SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => false,
                SystemFunctionKind::Clog2(input)
                | SystemFunctionKind::Onehot(input)
                | SystemFunctionKind::Signed(input)
                | SystemFunctionKind::Unsigned(input) => input_has_side_effect(input),
                // These are rejected in expression position, but classifying
                // them as effectful keeps eager lowering from becoming valid by
                // accident if that restriction changes.
                SystemFunctionKind::Readmemh(_, _)
                | SystemFunctionKind::Display(_)
                | SystemFunctionKind::Write(_)
                | SystemFunctionKind::Assert { .. }
                | SystemFunctionKind::Finish => true,
            },
            Factor::HierVariable(_) => false,
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => false,
        },
        Expression::Binary(lhs, _, rhs, _) => {
            expression_has_side_effect(lhs) || expression_has_side_effect(rhs)
        }
        Expression::Unary(_, inner, _) => expression_has_side_effect(inner),
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            expression_has_side_effect(cond)
                || expression_has_side_effect(then_expr)
                || expression_has_side_effect(else_expr)
        }
        Expression::Concatenation(items, _) => items.iter().any(|(expr, repeat)| {
            expression_has_side_effect(expr)
                || repeat.as_ref().is_some_and(expression_has_side_effect)
        }),
        Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
            ArrayLiteralItem::Value(expr, repeat) => {
                expression_has_side_effect(expr)
                    || repeat.as_deref().is_some_and(expression_has_side_effect)
            }
            ArrayLiteralItem::Defaul(expr) => expression_has_side_effect(expr),
        }),
        Expression::StructConstructor(_, fields, _) => fields
            .iter()
            .any(|(_, expr)| expression_has_side_effect(expr)),
    }
}

impl<'a> FfParser<'a> {
    pub(super) fn op_constant<A>(
        &mut self,
        v: SIRValue,
        width: usize,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let reg = if v.mask.is_zero() {
            ir_builder.alloc_bit(width, false)
        } else {
            ir_builder.alloc_logic(width)
        };

        ir_builder.emit(SIRInstruction::Imm(reg, v));
        self.stack.push_back(reg);
    }

    pub(super) fn parse_factor<A>(
        &mut self,
        factor: &Factor,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context: Option<ValueContext>,
    ) -> Result<(), ParserError> {
        let context_width = context.map(|context| context.width);
        match factor {
            Factor::Variable(var_id, var_index, var_select, comptime) => {
                // Compile-time constant parameter: emit as constant instead of loading
                // from memory (parameters are not stored in simulation memory).
                if comptime.is_const {
                    let is_bare =
                        var_index.0.is_empty() && var_select.0.is_empty() && var_select.1.is_none();
                    if is_bare {
                        if let Some((celox_value, mask_xz, width, _)) =
                            celox_value_from_comptime_in_context(comptime, context_width)
                        {
                            self.op_constant(
                                SIRValue::new_four_state(celox_value, mask_xz),
                                width,
                                ir_builder,
                            );
                            if let Some(context) = context {
                                let src = self.stack.pop_back().unwrap();
                                let adjusted = self.cast_reg_width_ext(
                                    ir_builder,
                                    src,
                                    context.width,
                                    context.signed,
                                );
                                self.stack.push_back(adjusted);
                            }
                            return Ok(());
                        }
                    } else if is_static_access(var_index, var_select) {
                        if let Some((celox_value, mask_xz, _full_width, _)) =
                            celox_value_from_comptime(comptime)
                        {
                            if let Ok(access) =
                                eval_var_select(self.module, *var_id, var_index, var_select)
                            {
                                let extracted_width = access.msb - access.lsb + 1;
                                let mask =
                                    (BigUint::from(1u64) << extracted_width) - BigUint::from(1u64);
                                let extracted_val = (&celox_value >> access.lsb) & &mask;
                                let extracted_mask = (&mask_xz >> access.lsb) & &mask;
                                self.op_constant(
                                    SIRValue::new_four_state(extracted_val, extracted_mask),
                                    extracted_width,
                                    ir_builder,
                                );
                                if let Some(context) = context {
                                    let src = self.stack.pop_back().unwrap();
                                    let adjusted = self.cast_reg_width_ext(
                                        ir_builder,
                                        src,
                                        context.width,
                                        context.signed,
                                    );
                                    self.stack.push_back(adjusted);
                                }
                                return Ok(());
                            }
                        }
                    }
                }
                if Self::is_function_state_base(comptime) {
                    self.op_load(
                        *var_id, var_index, var_select, domain, convert, sources, ir_builder,
                    )?;
                    if let Some(context) = context {
                        let src = self.stack.pop_back().unwrap();
                        let adjusted =
                            self.cast_reg_width_ext(ir_builder, src, context.width, context.signed);
                        self.stack.push_back(adjusted);
                    }
                    return Ok(());
                }
                if let Some(bound_reg) = self.get_bound_function_arg_value(*var_id) {
                    let bound_reg = *bound_reg;
                    if var_index.0.is_empty() && var_select.0.is_empty() && var_select.1.is_none() {
                        self.stack.push_back(bound_reg);
                        if let Some(context) = context {
                            let adjusted = self.cast_reg_width_ext(
                                ir_builder,
                                bound_reg,
                                context.width,
                                context.signed,
                            );
                            self.stack.pop_back();
                            self.stack.push_back(adjusted);
                        }
                        return Ok(());
                    }

                    let selected = self.emit_register_select(
                        bound_reg, *var_id, var_index, var_select, domain, convert, sources,
                        ir_builder,
                    )?;
                    self.stack.push_back(selected);
                    if let Some(context) = context {
                        let adjusted = self.cast_reg_width_ext(
                            ir_builder,
                            selected,
                            context.width,
                            context.signed,
                        );
                        self.stack.pop_back();
                        self.stack.push_back(adjusted);
                    }
                    return Ok(());
                }

                if let Some(frame) =
                    (0..self.frames.function_arg_stack.len())
                        .rev()
                        .find(|&frame| {
                            self.frames.function_array_view_enabled_stack[frame]
                                && self.frames.function_arg_stack[frame].contains_key(var_id)
                        })
                {
                    if self.array_access_needs_view(*var_id, var_index, var_select) {
                        self.ensure_function_array_view_at(
                            frame, *var_id, targets, domain, convert, sources, ir_builder,
                        )?;
                    }
                }
                if let Some(backing_var_id) = self.get_bound_function_array_view(*var_id) {
                    self.load_bound_array_literal_view(
                        *var_id,
                        backing_var_id,
                        var_index,
                        var_select,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    if let Some(context) = context {
                        let selected = self.stack.pop_back().unwrap();
                        let adjusted = self.cast_reg_width_ext(
                            ir_builder,
                            selected,
                            context.width,
                            context.signed,
                        );
                        self.stack.push_back(adjusted);
                    }
                    return Ok(());
                }

                if let Some(bound_expr) = self.get_bound_function_arg_expr(*var_id) {
                    let bound_expr = bound_expr.clone();
                    if var_index.0.is_empty() && var_select.0.is_empty() && var_select.1.is_none() {
                        let formal_width =
                            resolve_total_width(self.module, &self.module.variables[var_id])?;
                        self.materialize_bound_function_access(
                            *var_id,
                            &bound_expr,
                            BitAccess::new(0, formal_width - 1),
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                        if let Some(context) = context {
                            let formal = self.stack.pop_back().unwrap();
                            let adjusted = self.cast_reg_width_ext(
                                ir_builder,
                                formal,
                                context.width,
                                context.signed,
                            );
                            self.stack.push_back(adjusted);
                        }
                        return Ok(());
                    }

                    if let Expression::ArrayLiteral(items, _) = &bound_expr
                        && self.materialize_bound_array_literal_access(
                            *var_id, items, var_index, var_select, targets, domain, convert,
                            sources, ir_builder,
                        )?
                    {
                        return Ok(());
                    }

                    let Expression::Term(bound_factor) = &bound_expr else {
                        let Some(access) =
                            self.eval_formal_type_select(*var_id, var_index, var_select)
                        else {
                            return self.load_dynamic_bound_function_access(
                                *var_id,
                                &bound_expr,
                                var_index,
                                var_select,
                                targets,
                                domain,
                                convert,
                                sources,
                                ir_builder,
                            );
                        };
                        self.materialize_bound_function_access(
                            *var_id,
                            &bound_expr,
                            access,
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                        return Ok(());
                    };

                    let Factor::Variable(
                        bound_var_id,
                        bound_var_index,
                        bound_var_select,
                        bound_comptime,
                    ) = bound_factor.as_ref()
                    else {
                        let Some(access) =
                            self.eval_formal_type_select(*var_id, var_index, var_select)
                        else {
                            return self.load_dynamic_bound_function_access(
                                *var_id,
                                &bound_expr,
                                var_index,
                                var_select,
                                targets,
                                domain,
                                convert,
                                sources,
                                ir_builder,
                            );
                        };
                        self.materialize_bound_function_access(
                            *var_id,
                            &bound_expr,
                            access,
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                        return Ok(());
                    };

                    if bound_var_select.1.is_some() {
                        let Some(access) =
                            self.eval_formal_type_select(*var_id, var_index, var_select)
                        else {
                            return self.load_dynamic_bound_function_access(
                                *var_id,
                                &bound_expr,
                                var_index,
                                var_select,
                                targets,
                                domain,
                                convert,
                                sources,
                                ir_builder,
                            );
                        };
                        self.materialize_bound_function_access(
                            *var_id,
                            &bound_expr,
                            access,
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                        return Ok(());
                    }

                    let mut merged_index = bound_var_index.clone();
                    merged_index.append(var_index);

                    let mut merged_select = bound_var_select.clone();
                    merged_select.0.extend(var_select.0.iter().cloned());
                    merged_select.1 = var_select.1.clone();

                    if let Some(backing_var_id) = self.get_bound_function_array_view(*bound_var_id)
                    {
                        self.load_bound_array_literal_view(
                            *bound_var_id,
                            backing_var_id,
                            &merged_index,
                            &merged_select,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                    } else if self.get_bound_function_arg_expr(*bound_var_id).is_some() {
                        // Resolve forwarded formals recursively so a nested
                        // static access keeps the original literal's lazy
                        // direct-selection path.
                        self.parse_factor(
                            &Factor::Variable(
                                *bound_var_id,
                                merged_index,
                                merged_select,
                                bound_comptime.clone(),
                            ),
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                            None,
                        )?;
                        let forwarded = self.stack.pop_back().unwrap();
                        let access_width =
                            get_access_width(self.module, *var_id, var_index, var_select)?;
                        let formal = &self.module.variables[var_id];
                        let forwarded = self.coerce_register_to_formal(
                            ir_builder,
                            forwarded,
                            access_width,
                            bound_comptime.r#type.signed,
                            formal.r#type.signed,
                            formal.r#type.is_2state(),
                        );
                        self.stack.push_back(forwarded);
                    } else {
                        self.op_load(
                            *bound_var_id,
                            &merged_index,
                            &merged_select,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                    }
                } else {
                    self.op_load(
                        *var_id, var_index, var_select, domain, convert, sources, ir_builder,
                    )?;
                }
            }
            Factor::HierVariable(reference) => {
                return Err(ParserError::illegal_context(
                    "hierarchical variable reference",
                    format!(
                        "`{}` is only valid in a native testbench block",
                        reference.var_path
                    ),
                    Some(&reference.comptime.token),
                ));
            }
            Factor::Value(comptime) => {
                let (celox_value, mask_xz, width, _) =
                    celox_value_from_comptime_in_context(comptime, context_width)
                        .expect("Factor::Value should always have a numeric value");
                self.op_constant(
                    SIRValue::new_four_state(celox_value, mask_xz),
                    width,
                    ir_builder,
                );
            }
            Factor::SystemFunctionCall(call) => {
                self.parse_system_function_call(
                    call, targets, domain, convert, sources, ir_builder,
                )?;
            }
            Factor::FunctionCall(call) => {
                self.parse_function_call_expr(call, targets, domain, convert, sources, ir_builder)?;
            }
            Factor::Anonymous(comptime) | Factor::Unknown(comptime) => {
                return Err(ParserError::internal(
                    "unresolved factor in FF expression",
                    format!("{factor:?}"),
                    Some(&comptime.token),
                ));
            }
        }

        // Apply context_width adjustment
        if let Some(context) = context {
            let src_reg = self.stack.pop_back().unwrap();
            let adjusted =
                self.cast_reg_width_ext(ir_builder, src_reg, context.width, context.signed);
            self.stack.push_back(adjusted);
        }

        Ok(())
    }

    pub(super) fn parse_expression<A>(
        &mut self,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context_width: Option<usize>,
    ) -> Result<(), ParserError> {
        let context = context_width.map(|width| ValueContext {
            width,
            signed: expression_signed(expr),
        });
        self.parse_expression_in_context(
            expr, targets, domain, convert, sources, ir_builder, context,
        )
    }

    pub(super) fn parse_expression_in_context<A>(
        &mut self,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context: Option<ValueContext>,
    ) -> Result<(), ParserError> {
        let state = self
            .get_bound_function_event_arg_state(expr.token_range())
            .cloned();
        let has_state = state.is_some();
        if let Some(state) = state {
            self.frames.function_arg_stack.push(state);
            self.frames
                .function_array_view_stack
                .push(HashMap::default());
            self.frames.function_array_view_enabled_stack.push(false);
        }
        let result = self.parse_expression_in_context_inner(
            expr, targets, domain, convert, sources, ir_builder, context,
        );
        if has_state
            && result.is_ok()
            && let Some(value) = self.stack.back().cloned()
        {
            self.frames
                .function_expression_value_stack
                .last_mut()
                .expect("Function expression value scope is active")
                .insert(expr.token_range(), value);
        }
        if has_state {
            self.frames.function_array_view_enabled_stack.pop();
            self.frames.function_array_view_stack.pop();
            self.frames.function_arg_stack.pop();
        }
        result
    }

    fn parse_expression_in_context_inner<A>(
        &mut self,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context: Option<ValueContext>,
    ) -> Result<(), ParserError> {
        let context = if crate::float::precision(crate::float::type_of(expr)).is_some() {
            None
        } else {
            context
        };
        let context_width = context.map(|context| context.width);
        if let Some(bound_reg) = self
            .get_bound_function_expression_value(expr.token_range())
            .copied()
        {
            let result = if let Some(context) = context {
                self.cast_reg_width_ext(ir_builder, bound_reg, context.width, context.signed)
            } else {
                bound_reg
            };
            self.stack.push_back(result);
            return Ok(());
        }
        // Short-circuit: compile-time constant compound expression → emit constant value.
        // Unlike the SLT path (comb.rs), the SIR path requires the register width to
        // match context_width because emit_multi_dst_assign assumes rhs_width >= part_width.
        if !matches!(expr, Expression::Term(_)) {
            let ct = expr.comptime();
            if ct.is_const
                && !crate::float::needs_numeric_lowering(expr)
                && crate::float::type_of(expr).kind == ct.r#type.kind
            {
                if let Some((celox_value, mask_xz, width, _)) =
                    celox_value_from_comptime_in_context(ct, context_width)
                {
                    self.op_constant(
                        SIRValue::new_four_state(celox_value, mask_xz),
                        width,
                        ir_builder,
                    );
                    if let Some(context) = context {
                        let src = self.stack.pop_back().unwrap();
                        let adjusted =
                            self.cast_reg_width_ext(ir_builder, src, context.width, context.signed);
                        self.stack.push_back(adjusted);
                    }
                    return Ok(());
                }
            }
        }

        match expr {
            Expression::Term(factor) => {
                self.parse_factor(
                    factor, targets, domain, convert, sources, ir_builder, context,
                )?;
            }
            Expression::Binary(left, op, right, _) => {
                if *op == Op::As {
                    if let Some(op) = crate::float::conversion(
                        crate::float::type_of(left),
                        crate::float::type_of(expr),
                    ) {
                        self.parse_expression_in_context(
                            left, targets, domain, convert, sources, ir_builder, None,
                        )?;
                        let source = self.stack.pop_back().unwrap();
                        let result = ir_builder.alloc_bit(
                            crate::float::type_of(expr).total_width().unwrap(),
                            expr.comptime().r#type.signed,
                        );
                        ir_builder.emit(SIRInstruction::Unary(result, op, source));
                        self.stack.push_back(result);
                        return Ok(());
                    }
                } else if crate::float::precision(crate::float::type_of(left)).is_some()
                    || crate::float::precision(crate::float::type_of(right)).is_some()
                {
                    let target = if matches!(
                        crate::float::precision(crate::float::type_of(left)),
                        Some(celox_design::FloatPrecision::F64)
                    ) {
                        crate::float::type_of(left)
                    } else if crate::float::precision(crate::float::type_of(right)).is_some() {
                        crate::float::type_of(right)
                    } else {
                        crate::float::type_of(left)
                    };
                    let precision = crate::float::precision(target).unwrap();
                    self.parse_expression_in_context(
                        &crate::float::converted(left, target),
                        targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                        None,
                    )?;
                    let left = self.stack.pop_back().unwrap();
                    self.parse_expression_in_context(
                        &crate::float::converted(right, target),
                        targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                        None,
                    )?;
                    let right = self.stack.pop_back().unwrap();
                    let result = ir_builder
                        .alloc_bit(crate::float::type_of(expr).total_width().unwrap(), false);
                    ir_builder.emit(SIRInstruction::Binary(
                        result,
                        left,
                        BinaryOp::Float {
                            op: crate::float::binary(*op).expect("validated float operator"),
                            precision,
                        },
                        right,
                    ));
                    self.stack.push_back(result);
                    return Ok(());
                }
                self.parse_binary(
                    op, left, right, targets, domain, convert, sources, ir_builder, context,
                )?;
            }
            Expression::Unary(op, expr, _) => {
                if let Some(precision) = crate::float::precision(crate::float::type_of(expr)) {
                    self.parse_expression_in_context(
                        expr, targets, domain, convert, sources, ir_builder, None,
                    )?;
                    if *op == Op::Sub {
                        let source = self.stack.pop_back().unwrap();
                        let result = ir_builder
                            .alloc_bit(crate::float::type_of(expr).total_width().unwrap(), false);
                        ir_builder.emit(SIRInstruction::Unary(
                            result,
                            UnaryOp::Float {
                                op: celox_design::FloatUnaryOp::Neg,
                                precision,
                            },
                            source,
                        ));
                        self.stack.push_back(result);
                    }
                    return Ok(());
                }
                self.parse_unary(
                    op, expr, targets, domain, convert, sources, ir_builder, context,
                )?;
            }
            Expression::Ternary(cond, then, els, _) => {
                let target_type = crate::float::type_of(expr);
                let then = crate::float::converted(then, target_type);
                let els = crate::float::converted(els, target_type);
                self.parse_ternary(
                    cond, &then, &els, targets, domain, convert, sources, ir_builder, context,
                )?;
            }
            Expression::Concatenation(exprs, _) => {
                self.parse_concatenation(exprs, targets, domain, convert, sources, ir_builder)?;
            }
            Expression::ArrayLiteral(items, _) => {
                self.parse_array_literal(
                    items,
                    context_width,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
            }
            Expression::StructConstructor(ty, fields, _) => {
                self.parse_struct_constructor(
                    ty,
                    fields,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                    context_width,
                )?;
            }
        }
        if matches!(
            expr,
            Expression::Concatenation(..)
                | Expression::ArrayLiteral(..)
                | Expression::StructConstructor(..)
        ) && let Some(context) = context
        {
            let result = self.stack.pop_back().unwrap();
            let result = self.cast_reg_width_ext(ir_builder, result, context.width, context.signed);
            self.stack.push_back(result);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
