use super::{Domain, FfParser, FunctionArrayLiteralItemCache, FunctionArrayView};
use crate::context_width::{
    ValueContext, binary_semantics, cast_semantics, expression_signed, resolve_binary_op,
};
use crate::{
    HashMap, HashSet, LoweringPhase, ParserError,
    bitaccess::{
        celox_value_from_comptime, celox_value_from_comptime_in_context, eval_var_select,
        get_access_width, is_static_access,
    },
    function_call_arg, resolve_total_width,
};
use celox_design::{
    BinaryOp, BitAccess, SPARSE_WORKING_REGION, STABLE_REGION, UnaryOp, VarAtomBase, WORKING_REGION,
};
use celox_sir::{
    RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator, SIRValue,
};
use num_bigint::BigUint;
use num_traits::Zero;

use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, AssignStatement, Expression, Factor, Op,
    SystemFunctionCall, SystemFunctionKind, Type, ValueVariant, VarId, VarIndex, VarKind,
    VarSelect, VarSelectOp,
};
use veryl_analyzer::symbol::Affiliation;

fn expression_has_side_effect(expr: &Expression) -> bool {
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
                !call.outputs.is_empty() || call.inputs.values().any(expression_has_side_effect)
            }
            Factor::SystemFunctionCall(call) => match &call.kind {
                SystemFunctionKind::Bits(input)
                | SystemFunctionKind::Size(input)
                | SystemFunctionKind::Clog2(input)
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

#[derive(Clone, Copy)]
struct ArrayViewLayout {
    element_count: usize,
    element_width: usize,
    signed: bool,
    is_2state: bool,
}

#[derive(Clone)]
struct ArrayLiteralSelection<'a> {
    expr: &'a Expression,
    element_index: usize,
    element_count: usize,
    cache_key: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ArrayViewKey {
    frame: usize,
    var_id: VarId,
}

struct ArrayViewMergeCandidate {
    key: ArrayViewKey,
    needs_view: bool,
    cached_literal_items: HashSet<Vec<usize>>,
}

struct ArrayViewMergeParams {
    key: ArrayViewKey,
    initialized: RegisterId,
    elements: Vec<RegisterId>,
    cached_literal_items: Vec<ArrayLiteralItemMergeParams>,
}

struct ArrayLiteralItemMergeParams {
    cache_key: Vec<usize>,
    initialized: RegisterId,
    elements: Vec<RegisterId>,
}

#[derive(Default)]
struct FunctionInputUsage {
    runtime_reads: HashSet<VarId>,
    array_views: HashSet<VarId>,
}

fn add_offset_term<A>(
    accumulator: &mut Option<RegisterId>,
    term: RegisterId,
    builder: &mut SIRBuilder<A>,
) {
    if let Some(current) = *accumulator {
        let next = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Binary(next, current, BinaryOp::Add, term));
        *accumulator = Some(next);
    } else {
        *accumulator = Some(term);
    }
}

fn add_offset_constant<A>(
    accumulator: &mut Option<RegisterId>,
    value: u64,
    builder: &mut SIRBuilder<A>,
) {
    if value == 0 {
        return;
    }
    let constant = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(constant, SIRValue::new(value)));
    add_offset_term(accumulator, constant, builder);
}

fn scale_offset<A>(value: RegisterId, scale: usize, builder: &mut SIRBuilder<A>) -> RegisterId {
    if scale == 1 {
        return value;
    }
    let scale_reg = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(scale_reg, SIRValue::new(scale as u64)));
    let result = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Binary(
        result,
        value,
        BinaryOp::Mul,
        scale_reg,
    ));
    result
}

impl<'a> FfParser<'a> {
    fn eval_type_select(
        &self,
        typ: &Type,
        index: &VarIndex,
        select: &VarSelect,
    ) -> Option<BitAccess> {
        let mut dims: Vec<usize> = typ.array.iter().copied().collect::<Option<Vec<_>>>()?;
        if typ.width().is_empty() {
            if let Some(kind_width) = typ.kind.width() {
                dims.push(kind_width);
            }
        } else {
            dims.extend(typ.width().iter().copied().collect::<Option<Vec<_>>>()?);
        }

        let mut strides = vec![1; dims.len()];
        let mut current_stride = 1usize;
        for i in (0..dims.len()).rev() {
            strides[i] = current_stride;
            current_stride *= dims[i];
        }
        let total_width = current_stride;

        let to_u = |e: &Expression| {
            self.get_constant_value(e)
                .or_else(|| {
                    crate::bitaccess::eval_constexpr(e)
                        .and_then(|v| v.to_u64_digits().first().copied())
                })
                .map(|v| v as usize)
        };

        let get_slice_fallback = |base: usize, i: usize| -> BitAccess {
            let width = if i == 0 { total_width } else { strides[i - 1] };
            BitAccess::new(base, base + width - 1)
        };

        let mut all_indices = index.0.clone();
        all_indices.extend(select.0.iter().cloned());

        let mut base_offset = 0usize;
        let mut processed_count = 0usize;
        let limit = if select.1.is_some() {
            all_indices.len().saturating_sub(1)
        } else {
            all_indices.len()
        };

        for (i, index_val) in all_indices[..limit].iter().enumerate() {
            let idx = to_u(index_val)?;
            let stride = *strides.get(i)?;
            base_offset += idx * stride;
            processed_count += 1;
        }

        if let Some((op, range_expr)) = &select.1 {
            let anchor_expr = all_indices.last()?;
            let anchor = to_u(anchor_expr)?;
            let val = to_u(range_expr)?;
            let weight = *strides.get(processed_count).unwrap_or(&1);
            let (lsb_rel, msb_rel) = match op {
                VarSelectOp::Colon => (val * weight, anchor * weight + (weight - 1)),
                VarSelectOp::PlusColon => (anchor * weight, (anchor + val) * weight - 1),
                VarSelectOp::MinusColon => {
                    let msb = anchor * weight + (weight - 1);
                    let lsb = (anchor + 1).checked_sub(val)? * weight;
                    (lsb, msb)
                }
                VarSelectOp::Step => {
                    let actual_lsb = anchor * val;
                    let actual_msb = actual_lsb + val - 1;
                    (actual_lsb * weight, (actual_msb + 1) * weight - 1)
                }
            };
            Some(BitAccess::new(base_offset + lsb_rel, base_offset + msb_rel))
        } else if processed_count == dims.len() {
            Some(BitAccess::new(base_offset, base_offset))
        } else {
            Some(get_slice_fallback(base_offset, processed_count))
        }
    }

    fn emit_register_slice<A>(
        &mut self,
        src_reg: RegisterId,
        access: BitAccess,
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        let src_width = ir_builder.register(&src_reg).width();
        if access.lsb == 0 && access.msb + 1 == src_width {
            return src_reg;
        }

        let slice_width = access.msb - access.lsb + 1;
        let shifted_reg = if access.lsb == 0 {
            src_reg
        } else {
            let shift_amt_reg = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Imm(
                shift_amt_reg,
                SIRValue::new(access.lsb as u64),
            ));
            let shifted_reg = ir_builder.alloc_logic(src_width);
            ir_builder.emit(SIRInstruction::Binary(
                shifted_reg,
                src_reg,
                BinaryOp::Shr,
                shift_amt_reg,
            ));
            shifted_reg
        };

        if slice_width == src_width && access.lsb == 0 {
            shifted_reg
        } else {
            let mask_val = (BigUint::from(1u64) << slice_width) - BigUint::from(1u64);
            let mask_reg = ir_builder.alloc_bit(slice_width, false);
            ir_builder.emit(SIRInstruction::Imm(mask_reg, SIRValue::new(mask_val)));
            let sliced_reg = if ir_builder.register(&src_reg).is_signed() {
                ir_builder.alloc_bit(slice_width, true)
            } else {
                ir_builder.alloc_logic(slice_width)
            };
            ir_builder.emit(SIRInstruction::Binary(
                sliced_reg,
                shifted_reg,
                BinaryOp::And,
                mask_reg,
            ));
            sliced_reg
        }
    }

    fn emit_register_dynamic_slice<A>(
        &mut self,
        src_reg: RegisterId,
        offset_reg: RegisterId,
        width: usize,
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        let src_width = ir_builder.register(&src_reg).width();
        let shifted = ir_builder.alloc_logic(src_width);
        ir_builder.emit(SIRInstruction::Binary(
            shifted,
            src_reg,
            BinaryOp::Shr,
            offset_reg,
        ));
        if width == src_width {
            return shifted;
        }

        let mask = ir_builder.alloc_bit(width, false);
        ir_builder.emit(SIRInstruction::Imm(
            mask,
            SIRValue::new((BigUint::from(1u64) << width) - BigUint::from(1u64)),
        ));
        let selected = if ir_builder.register(&src_reg).is_signed() {
            ir_builder.alloc_bit(width, true)
        } else {
            ir_builder.alloc_logic(width)
        };
        ir_builder.emit(SIRInstruction::Binary(
            selected,
            shifted,
            BinaryOp::And,
            mask,
        ));
        selected
    }

    fn materialize_bound_function_access<A>(
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

    fn coerce_register_to_formal<A>(
        &self,
        ir_builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        target_width: usize,
        extend_signed: bool,
        result_signed: bool,
        target_is_2state: bool,
    ) -> RegisterId {
        let widened = self.cast_reg_width_ext(ir_builder, reg, target_width, extend_signed);
        if target_is_2state {
            match ir_builder.register(&widened) {
                RegisterType::Bit { width, signed }
                    if *width == target_width && *signed == result_signed =>
                {
                    widened
                }
                RegisterType::Bit { .. } => {
                    let bit_reg = ir_builder.alloc_bit(target_width, result_signed);
                    ir_builder.emit(SIRInstruction::Unary(bit_reg, UnaryOp::Ident, widened));
                    bit_reg
                }
                RegisterType::Logic { .. } => {
                    let bit_reg = ir_builder.alloc_bit(target_width, result_signed);
                    ir_builder.emit(SIRInstruction::Unary(bit_reg, UnaryOp::ToTwoState, widened));
                    bit_reg
                }
            }
        } else if matches!(ir_builder.register(&widened), RegisterType::Logic { .. }) {
            widened
        } else {
            // Four-state signedness is carried by the expression context. A
            // Bit register here would incorrectly discard the formal's X/Z
            // state merely to encode its signed flag.
            let logic_reg = ir_builder.alloc_logic(target_width);
            ir_builder.emit(SIRInstruction::Unary(logic_reg, UnaryOp::Ident, widened));
            logic_reg
        }
    }

    fn array_view_layout(&self, var_id: VarId) -> Result<ArrayViewLayout, ParserError> {
        let Some(variable) = self.module.variables.get(&var_id) else {
            unreachable!("validated function argument must have a formal variable");
        };
        if variable.r#type.array.is_empty() {
            unreachable!("array view requires an array variable");
        }
        let Some(element_count) = variable
            .r#type
            .array
            .iter()
            .copied()
            .try_fold(1usize, |total, dim| {
                dim.and_then(|dim| total.checked_mul(dim))
            })
        else {
            unreachable!("function argument validation resolves array dimensions without overflow");
        };
        let total_width = resolve_total_width(self.module, variable)?;
        if element_count == 0 || !total_width.is_multiple_of(element_count) {
            unreachable!("array has a nonzero element count that divides its width");
        }
        Ok(ArrayViewLayout {
            element_count,
            element_width: total_width / element_count,
            signed: variable.r#type.signed,
            is_2state: variable.r#type.is_2state(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn materialize_bound_array_literal_view<A>(
        &mut self,
        frame: usize,
        var_id: VarId,
        items: &[ArrayLiteralItem],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<FunctionArrayView, ParserError> {
        // Function bodies are lowered inline, so the formal's working region
        // is a call-scoped temporary. Populate it at the first access that
        // needs a complete view; control-flow joins carry its initialization
        // state and element snapshot explicitly.
        let Some(formal) = self.module.variables.get(&var_id) else {
            unreachable!("validated function argument must have a formal variable");
        };
        if formal.r#type.array.is_empty() {
            unreachable!("array literal view requires an array formal");
        }

        let array_dims: Option<Vec<usize>> = formal.r#type.array.iter().copied().collect();
        let Some(array_dims) = array_dims else {
            unreachable!("function argument shape validation resolves formal dimensions");
        };
        let layout = self.array_view_layout(var_id)?;

        let mut cached_elements = self.function_array_view_stack[frame]
            .get(&var_id)
            .map(|view| view.cached_literal_items.clone())
            .unwrap_or_default();
        let mut source_order = Vec::new();
        Self::collect_array_literal_elements_in_source_order(
            items,
            &mut Vec::new(),
            &mut source_order,
        );
        for (cache_key, selected_expr) in source_order {
            let element_count = selected_expr
                .comptime()
                .r#type
                .array
                .iter()
                .copied()
                .try_fold(1usize, |total, dim| {
                    dim.and_then(|dim| total.checked_mul(dim))
                })
                .unwrap_or_else(|| {
                    unreachable!(
                        "function argument validation resolves array literal item dimensions"
                    )
                });
            if let Some(cached) = cached_elements.get(&cache_key).cloned() {
                if let Some(initialized) = cached.initialized {
                    let array_view_candidates = self.array_view_merge_candidates([selected_expr]);
                    let array_view_params =
                        self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
                    let pre_array_views = self.function_array_view_stack.clone();
                    let evaluate_block = ir_builder.new_block();
                    let merged_elements = cached
                        .elements
                        .iter()
                        .map(|reg| ir_builder.alloc_reg(ir_builder.register(reg).clone()))
                        .collect::<Vec<_>>();
                    let mut merge_params = merged_elements.clone();
                    Self::append_array_view_merge_registers(&array_view_params, &mut merge_params);
                    let merge_block = ir_builder.new_block_with(merge_params);
                    let pre_defined = self.defined_ranges.clone();
                    let pre_dynamic = self.dynamic_defined_vars.clone();
                    let mut cached_args = cached.elements;
                    cached_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
                    ir_builder.seal_block(SIRTerminator::Branch {
                        cond: initialized,
                        true_block: (merge_block, cached_args),
                        false_block: (evaluate_block, vec![]),
                    });
                    ir_builder.switch_to_block(evaluate_block);
                    let mut evaluated = self.evaluate_array_literal_item(
                        selected_expr,
                        element_count,
                        layout,
                        targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    let evaluated_defined =
                        std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
                    let evaluated_dynamic =
                        std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
                    let evaluated_array_views = self.function_array_view_stack.clone();
                    evaluated.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
                    ir_builder.seal_block(SIRTerminator::Jump(merge_block, evaluated));
                    ir_builder.switch_to_block(merge_block);
                    self.function_array_view_stack = pre_array_views.clone();
                    self.install_merged_array_views(
                        &array_view_params,
                        &[&pre_array_views, &evaluated_array_views],
                    );
                    self.defined_ranges =
                        self.intersect_defined_states(pre_defined, evaluated_defined);
                    self.dynamic_defined_vars =
                        self.intersect_dynamic_vars(pre_dynamic, evaluated_dynamic);
                    cached_elements.insert(
                        cache_key,
                        FunctionArrayLiteralItemCache {
                            elements: merged_elements,
                            initialized: None,
                        },
                    );
                }
                continue;
            }
            let item_elements = self.evaluate_array_literal_item(
                selected_expr,
                element_count,
                layout,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            cached_elements.insert(
                cache_key,
                FunctionArrayLiteralItemCache {
                    elements: item_elements,
                    initialized: None,
                },
            );
        }

        let mut elements = Vec::with_capacity(layout.element_count);
        for linear_index in 0..layout.element_count {
            let mut remainder = linear_index;
            let mut coordinates = vec![0usize; array_dims.len()];
            for dimension in (0..array_dims.len()).rev() {
                coordinates[dimension] = remainder % array_dims[dimension];
                remainder /= array_dims[dimension];
            }

            let Some(selection) =
                self.select_array_literal_element(items, &coordinates, &array_dims)
            else {
                unreachable!("validated array literal covers every formal element");
            };
            let Some(selected_regs) = cached_elements.get(&selection.cache_key) else {
                unreachable!("every selected element was evaluated in literal source order");
            };
            if selected_regs.elements.len() != selection.element_count {
                unreachable!("selected array literal item has the validated element count");
            }
            let Some(&selected_reg) = selected_regs.elements.get(selection.element_index) else {
                unreachable!("selected array literal item index is in bounds");
            };
            elements.push(selected_reg);
        }

        self.store_array_view_elements(
            var_id,
            &elements,
            layout.element_width,
            convert,
            ir_builder,
        );

        Ok(FunctionArrayView {
            backing_var_id: var_id,
            elements,
            owns_backing: true,
            cached_literal_items: cached_elements,
            initialized: None,
        })
    }

    fn collect_array_literal_elements_in_source_order<'b>(
        items: &'b [ArrayLiteralItem],
        path: &mut Vec<usize>,
        elements: &mut Vec<(Vec<usize>, &'b Expression)>,
    ) {
        for (item_index, item) in items.iter().enumerate() {
            path.push(item_index);
            let expr = match item {
                ArrayLiteralItem::Value(expr, _) | ArrayLiteralItem::Defaul(expr) => expr.as_ref(),
            };
            if let Expression::ArrayLiteral(nested, _) = expr {
                Self::collect_array_literal_elements_in_source_order(nested, path, elements);
            } else {
                elements.push((path.clone(), expr));
            }
            path.pop();
        }
    }

    fn bound_array_literal_items_at(
        &self,
        frame: usize,
        var_id: VarId,
    ) -> Option<&[ArrayLiteralItem]> {
        match self.function_arg_stack[frame].get(&var_id)? {
            Expression::ArrayLiteral(items, _) => Some(items),
            Expression::Term(factor) => {
                let Factor::Variable(bound_var_id, index, select, _) = factor.as_ref() else {
                    return None;
                };
                if !index.0.is_empty() || !select.0.is_empty() || select.1.is_some() {
                    return None;
                }
                let source_frame = (0..frame).rev().find(|&source_frame| {
                    self.function_arg_stack[source_frame].contains_key(bound_var_id)
                })?;
                self.bound_array_literal_items_at(source_frame, *bound_var_id)
            }
            _ => None,
        }
    }

    fn array_literal_item_specs(&self, frame: usize, var_id: VarId) -> Vec<(Vec<usize>, usize)> {
        let Some(items) = self.bound_array_literal_items_at(frame, var_id) else {
            return Vec::new();
        };
        let mut source_order = Vec::new();
        Self::collect_array_literal_elements_in_source_order(
            items,
            &mut Vec::new(),
            &mut source_order,
        );
        source_order
            .into_iter()
            .map(|(cache_key, expr)| {
                let element_count = expr
                    .comptime()
                    .r#type
                    .array
                    .iter()
                    .copied()
                    .try_fold(1usize, |total, dim| {
                        dim.and_then(|dim| total.checked_mul(dim))
                    })
                    .unwrap_or_else(|| {
                        unreachable!(
                            "function argument validation resolves array literal item dimensions"
                        )
                    });
                (cache_key, element_count)
            })
            .collect()
    }

    fn array_literal_item_elements_from_view(
        &self,
        frame: usize,
        var_id: VarId,
        cache_key: &[usize],
        view_elements: &[RegisterId],
        element_count: usize,
    ) -> Option<Vec<RegisterId>> {
        let items = self.bound_array_literal_items_at(frame, var_id)?;
        let array_dims = self.module.variables[&var_id]
            .r#type
            .array
            .iter()
            .copied()
            .collect::<Option<Vec<_>>>()?;
        let mut elements = vec![None; element_count];
        for (linear_index, &view_element) in view_elements.iter().enumerate() {
            let mut remainder = linear_index;
            let mut coordinates = vec![0usize; array_dims.len()];
            for dimension in (0..array_dims.len()).rev() {
                coordinates[dimension] = remainder % array_dims[dimension];
                remainder /= array_dims[dimension];
            }
            let selection = self.select_array_literal_element(items, &coordinates, &array_dims)?;
            if selection.cache_key == cache_key {
                elements[selection.element_index] = Some(view_element);
            }
        }
        elements.into_iter().collect()
    }

    fn definite_array_literal_caches_from_view(
        &self,
        frame: usize,
        var_id: VarId,
        view_elements: &[RegisterId],
    ) -> HashMap<Vec<usize>, FunctionArrayLiteralItemCache> {
        self.array_literal_item_specs(frame, var_id)
            .into_iter()
            .filter_map(|(cache_key, element_count)| {
                let elements = self.array_literal_item_elements_from_view(
                    frame,
                    var_id,
                    &cache_key,
                    view_elements,
                    element_count,
                )?;
                Some((
                    cache_key,
                    FunctionArrayLiteralItemCache {
                        elements,
                        initialized: None,
                    },
                ))
            })
            .collect()
    }

    fn slice_array_value_element<A>(
        &self,
        reg: RegisterId,
        element_index: usize,
        element_count: usize,
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        if element_count == 0 || element_index >= element_count {
            unreachable!("validated array value element index is in bounds");
        }
        if element_count == 1 {
            return reg;
        }
        let source_width = ir_builder.register(&reg).width();
        if !source_width.is_multiple_of(element_count) {
            unreachable!("validated array value has a divisible nonzero width");
        }
        let source_element_width = source_width / element_count;
        let source_type = ir_builder.register(&reg).clone();
        let element = match source_type {
            RegisterType::Logic { .. } => ir_builder.alloc_logic(source_element_width),
            RegisterType::Bit { signed, .. } => ir_builder.alloc_bit(source_element_width, signed),
        };
        // Whole unpacked-array values use PackedElements order: coordinate
        // zero starts at the low end of the register.
        let bit_offset = element_index * source_element_width;
        ir_builder.emit(SIRInstruction::Slice(
            element,
            reg,
            bit_offset,
            source_element_width,
        ));
        element
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_array_literal_item<A>(
        &mut self,
        expr: &Expression,
        element_count: usize,
        layout: ArrayViewLayout,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Vec<RegisterId>, ParserError> {
        self.parse_expression(
            expr,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            (element_count == 1).then_some(layout.element_width),
        )?;
        let item_reg = self.stack.pop_back().unwrap();
        let mut elements = Vec::with_capacity(element_count);
        for element_index in 0..element_count {
            let element =
                self.slice_array_value_element(item_reg, element_index, element_count, ir_builder);
            elements.push(self.coerce_register_to_formal(
                ir_builder,
                element,
                layout.element_width,
                expr.comptime().r#type.signed,
                layout.signed,
                layout.is_2state,
            ));
        }
        Ok(elements)
    }

    fn store_array_view_elements<A>(
        &self,
        backing_var_id: VarId,
        elements: &[RegisterId],
        element_width: usize,
        convert: &impl Fn(VarId, u32) -> A,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        for (linear_index, &element) in elements.iter().enumerate() {
            let element_index = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Imm(
                element_index,
                SIRValue::new(linear_index as u64),
            ));
            ir_builder.emit(SIRInstruction::Store(
                convert(backing_var_id, WORKING_REGION),
                SIROffset::Element {
                    index: element_index,
                    element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
                element_width,
                element,
                Vec::new(),
                Vec::new(),
            ));
        }
    }

    fn materialize_converted_array_view<A>(
        &mut self,
        var_id: VarId,
        backing_var_id: VarId,
        convert: &impl Fn(VarId, u32) -> A,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<FunctionArrayView, ParserError> {
        // A view can alias an existing backing only when both formals use the
        // same element storage representation. Otherwise create a converted
        // temporary for the callee while preserving element-wise coercion.
        let target = self.array_view_layout(var_id)?;
        let source = self.array_view_layout(backing_var_id)?;
        if target.element_count != source.element_count {
            unreachable!("function argument shape validation preserves array element count");
        }

        let mut elements = Vec::with_capacity(target.element_count);
        for linear_index in 0..target.element_count {
            let element_index = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Imm(
                element_index,
                SIRValue::new(linear_index as u64),
            ));
            let source_reg = if source.is_2state {
                ir_builder.alloc_bit(source.element_width, source.signed)
            } else {
                ir_builder.alloc_logic(source.element_width)
            };
            ir_builder.emit(SIRInstruction::Load(
                source_reg,
                convert(backing_var_id, WORKING_REGION),
                SIROffset::Element {
                    index: element_index,
                    element_width: source.element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
                source.element_width,
            ));
            let converted = self.coerce_register_to_formal(
                ir_builder,
                source_reg,
                target.element_width,
                source.signed,
                target.signed,
                target.is_2state,
            );
            elements.push(converted);
        }
        self.store_array_view_elements(
            var_id,
            &elements,
            target.element_width,
            convert,
            ir_builder,
        );
        Ok(FunctionArrayView {
            backing_var_id: var_id,
            elements,
            owns_backing: true,
            cached_literal_items: HashMap::default(),
            initialized: None,
        })
    }

    fn array_access_needs_view(&self, var_id: VarId, index: &VarIndex, select: &VarSelect) -> bool {
        let Some(formal) = self.module.variables.get(&var_id) else {
            return false;
        };
        if formal.r#type.array.is_empty() {
            return false;
        }
        if select.1.is_some() {
            return true;
        }
        let Some(array_dims) = formal
            .r#type
            .array
            .iter()
            .copied()
            .collect::<Option<Vec<_>>>()
        else {
            return true;
        };
        let all_indices = index.0.iter().chain(&select.0).collect::<Vec<_>>();
        all_indices.len() != array_dims.len()
            || all_indices.iter().zip(array_dims).any(|(expr, dim)| {
                self.get_constant_value(expr)
                    .is_none_or(|value| value as usize >= dim)
            })
    }

    fn function_input_usage(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        active_calls: &mut HashSet<VarId>,
    ) -> Option<FunctionInputUsage> {
        if !active_calls.insert(call.id) {
            return None;
        }
        let result = (|| {
            let function = self.module.functions.get(&call.id)?;
            let function_body = if let Some(index) = &call.index {
                function.get_function(index)?
            } else {
                function.get_function(&[])?
            };
            let mut usage = FunctionInputUsage::default();
            for (arg_path, _) in &call.outputs {
                let arg_id = *function_body.arg_map.get(arg_path)?;
                let expr = self
                    .extract_function_target_expr(&function_body, arg_id, &HashMap::default())
                    .ok()?;
                self.collect_function_input_usage(&expr, &mut usage, active_calls)?;
            }
            if let Some(ret_id) = function_body.ret {
                let expr = self
                    .extract_function_return_expr(&function_body, ret_id)
                    .ok()?;
                self.collect_function_input_usage(&expr, &mut usage, active_calls)?;
            }
            let formals = function_body
                .arg_map
                .values()
                .copied()
                .collect::<HashSet<_>>();
            usage
                .runtime_reads
                .retain(|var_id| formals.contains(var_id));
            usage.array_views.retain(|var_id| formals.contains(var_id));
            Some(usage)
        })();
        active_calls.remove(&call.id);
        result
    }

    fn collect_function_input_usage(
        &self,
        expr: &Expression,
        usage: &mut FunctionInputUsage,
        active_calls: &mut HashSet<VarId>,
    ) -> Option<()> {
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(var_id, index, select, _) => {
                    usage.runtime_reads.insert(*var_id);
                    if self.array_access_needs_view(*var_id, index, select) {
                        usage.array_views.insert(*var_id);
                    }
                    for expr in &index.0 {
                        self.collect_function_input_usage(expr, usage, active_calls)?;
                    }
                    for expr in &select.0 {
                        self.collect_function_input_usage(expr, usage, active_calls)?;
                    }
                    if let Some((_, expr)) = &select.1 {
                        self.collect_function_input_usage(expr, usage, active_calls)?;
                    }
                }
                Factor::FunctionCall(call) => {
                    let nested_usage = self.function_input_usage(call, active_calls)?;
                    let function = self.module.functions.get(&call.id)?;
                    let function_body = if let Some(index) = &call.index {
                        function.get_function(index)?
                    } else {
                        function.get_function(&[])?
                    };
                    for (arg_path, arg_id) in &function_body.arg_map {
                        let formal = &self.module.variables[arg_id];
                        let needs_actual = if formal.r#type.array.is_empty() {
                            nested_usage.runtime_reads.contains(arg_id)
                        } else {
                            nested_usage.runtime_reads.contains(arg_id)
                                || nested_usage.array_views.contains(arg_id)
                        };
                        if needs_actual
                            && let Some(expr) = function_call_arg(&call.inputs, arg_path)
                        {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                        }
                    }
                    for dst in call.outputs.values().flatten() {
                        for expr in &dst.index.0 {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                        }
                        for expr in &dst.select.0 {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                        }
                        if let Some((_, expr)) = &dst.select.1 {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                        }
                    }
                }
                Factor::SystemFunctionCall(call) => match &call.kind {
                    SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => {}
                    SystemFunctionKind::Clog2(input)
                    | SystemFunctionKind::Onehot(input)
                    | SystemFunctionKind::Signed(input)
                    | SystemFunctionKind::Unsigned(input) => {
                        self.collect_function_input_usage(&input.0, usage, active_calls)?;
                    }
                    SystemFunctionKind::Readmemh(_, _)
                    | SystemFunctionKind::Display(_)
                    | SystemFunctionKind::Write(_)
                    | SystemFunctionKind::Assert { .. }
                    | SystemFunctionKind::Finish => {}
                },
                Factor::HierVariable(_) => return None,
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
            },
            Expression::Binary(lhs, _, rhs, _) => {
                self.collect_function_input_usage(lhs, usage, active_calls)?;
                self.collect_function_input_usage(rhs, usage, active_calls)?;
            }
            Expression::Unary(_, inner, _) => {
                self.collect_function_input_usage(inner, usage, active_calls)?;
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                self.collect_function_input_usage(cond, usage, active_calls)?;
                self.collect_function_input_usage(then_expr, usage, active_calls)?;
                self.collect_function_input_usage(else_expr, usage, active_calls)?;
            }
            Expression::Concatenation(items, _) => {
                for (expr, repeat) in items {
                    self.collect_function_input_usage(expr, usage, active_calls)?;
                    if let Some(repeat) = repeat {
                        self.collect_function_input_usage(repeat, usage, active_calls)?;
                    }
                }
            }
            Expression::ArrayLiteral(items, _) => {
                for item in items {
                    match item {
                        ArrayLiteralItem::Value(expr, repeat) => {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                            if let Some(repeat) = repeat {
                                self.collect_function_input_usage(repeat, usage, active_calls)?;
                            }
                        }
                        ArrayLiteralItem::Defaul(expr) => {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                        }
                    }
                }
            }
            Expression::StructConstructor(_, fields, _) => {
                for (_, expr) in fields {
                    self.collect_function_input_usage(expr, usage, active_calls)?;
                }
            }
        }
        Some(())
    }

    fn collect_array_views_for_expression(
        &self,
        expr: &Expression,
        candidates: &mut Vec<ArrayViewMergeCandidate>,
        candidate_indices: &mut HashMap<ArrayViewKey, usize>,
    ) {
        let collect = |expr, candidates: &mut Vec<_>, candidate_indices: &mut HashMap<_, _>| {
            self.collect_array_views_for_expression(expr, candidates, candidate_indices)
        };
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(var_id, index, select, _) => {
                    if self
                        .module
                        .variables
                        .get(var_id)
                        .is_some_and(|variable| !variable.r#type.array.is_empty())
                        && let Some(frame) = (0..self.function_arg_stack.len())
                            .rev()
                            .find(|&frame| self.function_arg_stack[frame].contains_key(var_id))
                    {
                        let key = ArrayViewKey {
                            frame,
                            var_id: *var_id,
                        };
                        let static_indices = self.static_array_view_indices(*var_id, index, select);
                        self.record_array_view_merge_candidate(
                            key,
                            static_indices.as_deref(),
                            candidates,
                            candidate_indices,
                        );
                        if let Some(bound_expr) = self.function_arg_stack[frame].get(var_id) {
                            self.collect_array_views_in_bound_expression(
                                frame,
                                *var_id,
                                bound_expr,
                                static_indices.as_deref(),
                                candidates,
                                candidate_indices,
                            );
                        }
                    }
                    for expr in &index.0 {
                        collect(expr, candidates, candidate_indices);
                    }
                    for expr in &select.0 {
                        collect(expr, candidates, candidate_indices);
                    }
                    if let Some((_, expr)) = &select.1 {
                        collect(expr, candidates, candidate_indices);
                    }
                }
                Factor::FunctionCall(call) => {
                    let input_usage = self.function_input_usage(call, &mut HashSet::default());
                    let function_body = self.module.functions.get(&call.id).and_then(|function| {
                        if let Some(index) = &call.index {
                            function.get_function(index)
                        } else {
                            function.get_function(&[])
                        }
                    });
                    if let (Some(input_usage), Some(function_body)) = (input_usage, function_body) {
                        for (arg_path, arg_id) in &function_body.arg_map {
                            let formal = &self.module.variables[arg_id];
                            let needs_actual = if formal.r#type.array.is_empty() {
                                input_usage.runtime_reads.contains(arg_id)
                            } else {
                                input_usage.runtime_reads.contains(arg_id)
                                    || input_usage.array_views.contains(arg_id)
                            };
                            if needs_actual
                                && let Some(expr) = function_call_arg(&call.inputs, arg_path)
                            {
                                collect(expr, candidates, candidate_indices);
                            }
                        }
                    } else {
                        // Unsupported or recursive callees will fail during
                        // normal lowering; keep the conservative traversal.
                        for expr in call.inputs.values() {
                            collect(expr, candidates, candidate_indices);
                        }
                    }
                    for dst in call.outputs.values().flatten() {
                        for expr in &dst.index.0 {
                            collect(expr, candidates, candidate_indices);
                        }
                        for expr in &dst.select.0 {
                            collect(expr, candidates, candidate_indices);
                        }
                        if let Some((_, expr)) = &dst.select.1 {
                            collect(expr, candidates, candidate_indices);
                        }
                    }
                }
                Factor::SystemFunctionCall(call) => match &call.kind {
                    // Reflection queries are compile-time constants and do
                    // not evaluate their operand during expression lowering.
                    SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => {}
                    SystemFunctionKind::Clog2(input)
                    | SystemFunctionKind::Onehot(input)
                    | SystemFunctionKind::Signed(input)
                    | SystemFunctionKind::Unsigned(input) => {
                        collect(&input.0, candidates, candidate_indices)
                    }
                    SystemFunctionKind::Readmemh(_, _)
                    | SystemFunctionKind::Display(_)
                    | SystemFunctionKind::Write(_)
                    | SystemFunctionKind::Assert { .. }
                    | SystemFunctionKind::Finish => {}
                },
                Factor::HierVariable(_) => {}
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
            },
            Expression::Binary(lhs, _, rhs, _) => {
                collect(lhs, candidates, candidate_indices);
                collect(rhs, candidates, candidate_indices);
            }
            Expression::Unary(_, inner, _) => collect(inner, candidates, candidate_indices),
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                collect(cond, candidates, candidate_indices);
                collect(then_expr, candidates, candidate_indices);
                collect(else_expr, candidates, candidate_indices);
            }
            Expression::Concatenation(items, _) => {
                for (expr, repeat) in items {
                    collect(expr, candidates, candidate_indices);
                    if let Some(repeat) = repeat {
                        collect(repeat, candidates, candidate_indices);
                    }
                }
            }
            Expression::ArrayLiteral(items, _) => {
                for item in items {
                    match item {
                        ArrayLiteralItem::Value(expr, repeat) => {
                            collect(expr, candidates, candidate_indices);
                            if let Some(repeat) = repeat {
                                collect(repeat, candidates, candidate_indices);
                            }
                        }
                        ArrayLiteralItem::Defaul(expr) => {
                            collect(expr, candidates, candidate_indices)
                        }
                    }
                }
            }
            Expression::StructConstructor(_, fields, _) => {
                for (_, expr) in fields {
                    collect(expr, candidates, candidate_indices);
                }
            }
        }
    }

    fn static_array_view_indices(
        &self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
    ) -> Option<Vec<usize>> {
        if self.array_access_needs_view(var_id, index, select) {
            return None;
        }
        index
            .0
            .iter()
            .chain(&select.0)
            .map(|expr| self.get_constant_value(expr).map(|value| value as usize))
            .collect()
    }

    fn record_array_view_merge_candidate(
        &self,
        key: ArrayViewKey,
        static_indices: Option<&[usize]>,
        candidates: &mut Vec<ArrayViewMergeCandidate>,
        candidate_indices: &mut HashMap<ArrayViewKey, usize>,
    ) {
        let candidate_index = if let Some(&candidate_index) = candidate_indices.get(&key) {
            candidate_index
        } else {
            let candidate_index = candidates.len();
            candidates.push(ArrayViewMergeCandidate {
                key,
                needs_view: false,
                cached_literal_items: HashSet::default(),
            });
            candidate_indices.insert(key, candidate_index);
            candidate_index
        };
        let candidate = &mut candidates[candidate_index];
        if let Some(static_indices) = static_indices {
            if let Some(items) = self.bound_array_literal_items_at(key.frame, key.var_id)
                && let Some(array_dims) = self.module.variables[&key.var_id]
                    .r#type
                    .array
                    .iter()
                    .copied()
                    .collect::<Option<Vec<_>>>()
                && let Some(selection) =
                    self.select_array_literal_element(items, static_indices, &array_dims)
            {
                candidate.cached_literal_items.insert(selection.cache_key);
            }
        } else {
            candidate.needs_view = true;
        }
    }

    fn collect_array_views_in_bound_expression(
        &self,
        frame: usize,
        var_id: VarId,
        expr: &Expression,
        static_indices: Option<&[usize]>,
        candidates: &mut Vec<ArrayViewMergeCandidate>,
        candidate_indices: &mut HashMap<ArrayViewKey, usize>,
    ) {
        if let Expression::Term(factor) = expr
            && let Factor::Variable(bound_var_id, index, select, _) = factor.as_ref()
            && index.0.is_empty()
            && select.0.is_empty()
            && select.1.is_none()
            && let Some(source_frame) = (0..frame).rev().find(|&source_frame| {
                self.function_arg_stack[source_frame].contains_key(bound_var_id)
            })
            && let Some(source_expr) = self.function_arg_stack[source_frame].get(bound_var_id)
        {
            let key = ArrayViewKey {
                frame: source_frame,
                var_id: *bound_var_id,
            };
            self.record_array_view_merge_candidate(
                key,
                static_indices,
                candidates,
                candidate_indices,
            );
            self.collect_array_views_in_bound_expression(
                source_frame,
                *bound_var_id,
                source_expr,
                static_indices,
                candidates,
                candidate_indices,
            );
        } else if let (Some(static_indices), Expression::ArrayLiteral(items, _)) =
            (static_indices, expr)
            && let Some(array_dims) = self.module.variables[&var_id]
                .r#type
                .array
                .iter()
                .copied()
                .collect::<Option<Vec<_>>>()
            && let Some(selection) =
                self.select_array_literal_element(items, static_indices, &array_dims)
        {
            self.collect_array_views_for_expression(selection.expr, candidates, candidate_indices);
        } else {
            self.collect_array_views_for_expression(expr, candidates, candidate_indices);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_function_array_view_at<A>(
        &mut self,
        frame: usize,
        var_id: VarId,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Option<FunctionArrayView>, ParserError> {
        let Some(bound_expr) = self.function_arg_stack[frame].get(&var_id).cloned() else {
            return Ok(None);
        };

        let view = match bound_expr {
            Expression::ArrayLiteral(items, _)
                if !self.module.variables[&var_id].r#type.array.is_empty() =>
            {
                Some(self.materialize_bound_array_literal_view(
                    frame, var_id, &items, targets, domain, convert, sources, ir_builder,
                )?)
            }
            Expression::Term(factor) => {
                let Factor::Variable(bound_var_id, index, select, _) = factor.as_ref() else {
                    return Ok(None);
                };
                if !index.0.is_empty() || !select.0.is_empty() || select.1.is_some() {
                    return Ok(None);
                }
                let Some(source_frame) = (0..frame)
                    .rev()
                    .find(|&i| self.function_arg_stack[i].contains_key(bound_var_id))
                else {
                    return Ok(None);
                };
                let Some(source_view) = self.ensure_function_array_view_at(
                    source_frame,
                    *bound_var_id,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?
                else {
                    return Ok(None);
                };
                let target = self.array_view_layout(var_id)?;
                let source = self.array_view_layout(*bound_var_id)?;
                if target.element_width == source.element_width
                    && target.is_2state == source.is_2state
                    && target.signed == source.signed
                {
                    Some(FunctionArrayView {
                        backing_var_id: source_view.backing_var_id,
                        elements: source_view.elements,
                        owns_backing: false,
                        cached_literal_items: source_view.cached_literal_items,
                        initialized: None,
                    })
                } else {
                    Some(self.materialize_converted_array_view(
                        var_id,
                        source_view.backing_var_id,
                        convert,
                        ir_builder,
                    )?)
                }
            }
            _ => None,
        };
        Ok(view)
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_function_array_view_at<A>(
        &mut self,
        frame: usize,
        var_id: VarId,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Option<FunctionArrayView>, ParserError> {
        if let Some(view) = self.function_array_view_stack[frame].get(&var_id).cloned() {
            let Some(initialized) = view.initialized else {
                return Ok(Some(view));
            };

            let bound_expr = self.function_arg_stack[frame]
                .get(&var_id)
                .unwrap_or_else(|| unreachable!("an array view has a bound argument"))
                .clone();
            let array_view_candidates = self.array_view_merge_candidates([&bound_expr]);
            let array_view_params =
                self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
            let pre_array_views = self.function_array_view_stack.clone();
            let pre_defined = self.defined_ranges.clone();
            let pre_dynamic = self.dynamic_defined_vars.clone();
            let initialize_block = ir_builder.new_block();
            let carried_elements = if view.elements.is_empty() {
                let layout = self.array_view_layout(var_id)?;
                (0..layout.element_count)
                    .map(|_| {
                        let element = if layout.is_2state {
                            ir_builder.alloc_bit(layout.element_width, layout.signed)
                        } else {
                            ir_builder.alloc_logic(layout.element_width)
                        };
                        ir_builder.emit(SIRInstruction::Imm(element, SIRValue::new(0u8)));
                        element
                    })
                    .collect::<Vec<_>>()
            } else {
                view.elements.clone()
            };
            let merged_elements = carried_elements
                .iter()
                .map(|reg| ir_builder.alloc_reg(ir_builder.register(reg).clone()))
                .collect::<Vec<_>>();
            let mut merge_params = merged_elements.clone();
            Self::append_array_view_merge_registers(&array_view_params, &mut merge_params);
            let merge_block = ir_builder.new_block_with(merge_params);
            let mut carried_args = carried_elements;
            carried_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: initialized,
                true_block: (merge_block, carried_args),
                false_block: (initialize_block, vec![]),
            });

            ir_builder.switch_to_block(initialize_block);
            let Some(initialized_view) = self.build_function_array_view_at(
                frame, var_id, targets, domain, convert, sources, ir_builder,
            )?
            else {
                unreachable!("a conditional array view has a materializable binding");
            };
            if initialized_view.backing_var_id != view.backing_var_id
                || initialized_view.owns_backing != view.owns_backing
            {
                unreachable!("a function array view has stable backing metadata");
            }
            let initialized_defined =
                std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
            let initialized_dynamic =
                std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
            let initialized_array_views = self.function_array_view_stack.clone();
            let mut initialized_args = initialized_view.elements;
            initialized_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, initialized_args));
            ir_builder.switch_to_block(merge_block);
            self.function_array_view_stack = pre_array_views.clone();
            self.install_merged_array_views(
                &array_view_params,
                &[&pre_array_views, &initialized_array_views],
            );
            self.defined_ranges = self.intersect_defined_states(pre_defined, initialized_defined);
            self.dynamic_defined_vars =
                self.intersect_dynamic_vars(pre_dynamic, initialized_dynamic);
            let cached_literal_items =
                self.definite_array_literal_caches_from_view(frame, var_id, &merged_elements);
            let view = FunctionArrayView {
                backing_var_id: view.backing_var_id,
                elements: merged_elements,
                owns_backing: view.owns_backing,
                cached_literal_items,
                initialized: None,
            };
            self.function_array_view_stack[frame].insert(var_id, view.clone());
            return Ok(Some(view));
        }

        let view = self.build_function_array_view_at(
            frame, var_id, targets, domain, convert, sources, ir_builder,
        )?;
        if let Some(view) = &view {
            self.function_array_view_stack[frame].insert(var_id, view.clone());
        }
        Ok(view)
    }

    fn array_view_merge_candidates<'b>(
        &self,
        expressions: impl IntoIterator<Item = &'b Expression>,
    ) -> Vec<ArrayViewMergeCandidate> {
        let mut candidates = Vec::new();
        let mut candidate_indices = HashMap::default();
        for expr in expressions {
            self.collect_array_views_for_expression(expr, &mut candidates, &mut candidate_indices);
        }
        candidates.retain(|candidate| {
            self.function_array_view_stack[candidate.key.frame]
                .get(&candidate.key.var_id)
                .is_none_or(|view| view.initialized.is_some())
        });
        candidates
    }

    fn alloc_array_view_merge_params<A>(
        &self,
        candidates: &[ArrayViewMergeCandidate],
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Vec<ArrayViewMergeParams>, ParserError> {
        candidates
            .iter()
            .map(|candidate| {
                let key = candidate.key;
                let layout = self.array_view_layout(candidate.key.var_id)?;
                let initialized = ir_builder.alloc_bit(1, false);
                let existing_view = self.function_array_view_stack[key.frame].get(&key.var_id);
                let carries_view = candidate.needs_view
                    || existing_view.is_some_and(|view| !view.elements.is_empty());
                let elements = (0..if carries_view {
                    layout.element_count
                } else {
                    0
                })
                    .map(|_| {
                        if layout.is_2state {
                            ir_builder.alloc_bit(layout.element_width, layout.signed)
                        } else {
                            ir_builder.alloc_logic(layout.element_width)
                        }
                    })
                    .collect();
                let existing_cache_keys = existing_view
                    .into_iter()
                    .flat_map(|view| view.cached_literal_items.keys())
                    .collect::<HashSet<_>>();
                let cached_literal_items = self
                    .array_literal_item_specs(key.frame, key.var_id)
                    .into_iter()
                    .filter(|(cache_key, _)| {
                        carries_view
                            || candidate.cached_literal_items.contains(cache_key)
                            || existing_cache_keys.contains(cache_key)
                    })
                    .map(|(cache_key, element_count)| ArrayLiteralItemMergeParams {
                        cache_key,
                        initialized: ir_builder.alloc_bit(1, false),
                        elements: (0..element_count)
                            .map(|_| {
                                if layout.is_2state {
                                    ir_builder.alloc_bit(layout.element_width, layout.signed)
                                } else {
                                    ir_builder.alloc_logic(layout.element_width)
                                }
                            })
                            .collect(),
                    })
                    .collect();
                Ok(ArrayViewMergeParams {
                    key,
                    initialized,
                    elements,
                    cached_literal_items,
                })
            })
            .collect()
    }

    fn append_array_view_merge_registers(
        params: &[ArrayViewMergeParams],
        registers: &mut Vec<RegisterId>,
    ) {
        for params in params {
            registers.push(params.initialized);
            registers.extend(params.elements.iter().copied());
            for cached in &params.cached_literal_items {
                registers.push(cached.initialized);
                registers.extend(cached.elements.iter().copied());
            }
        }
    }

    fn array_view_state_args<A>(
        &self,
        params: &[ArrayViewMergeParams],
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Vec<RegisterId>, ParserError> {
        let mut args = Vec::new();
        for params in params {
            if let Some(view) =
                self.function_array_view_stack[params.key.frame].get(&params.key.var_id)
            {
                let initialized = if let Some(initialized) = view.initialized {
                    initialized
                } else {
                    let initialized = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(1u8)));
                    initialized
                };
                args.push(initialized);
                if view.elements.is_empty() {
                    let layout = self.array_view_layout(params.key.var_id)?;
                    for _ in &params.elements {
                        let element = if layout.is_2state {
                            ir_builder.alloc_bit(layout.element_width, layout.signed)
                        } else {
                            ir_builder.alloc_logic(layout.element_width)
                        };
                        ir_builder.emit(SIRInstruction::Imm(element, SIRValue::new(0u8)));
                        args.push(element);
                    }
                } else {
                    if view.elements.len() != params.elements.len() {
                        unreachable!("a merged array view has the planned element count");
                    }
                    args.extend(view.elements.iter().copied());
                }
            } else {
                let initialized = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(0u8)));
                args.push(initialized);
                let layout = self.array_view_layout(params.key.var_id)?;
                for _ in &params.elements {
                    let element = if layout.is_2state {
                        ir_builder.alloc_bit(layout.element_width, layout.signed)
                    } else {
                        ir_builder.alloc_logic(layout.element_width)
                    };
                    ir_builder.emit(SIRInstruction::Imm(element, SIRValue::new(0u8)));
                    args.push(element);
                }
            }
            let view = self.function_array_view_stack[params.key.frame].get(&params.key.var_id);
            for cached_params in &params.cached_literal_items {
                if let Some(cached) =
                    view.and_then(|view| view.cached_literal_items.get(&cached_params.cache_key))
                {
                    let initialized = if let Some(initialized) = cached.initialized {
                        initialized
                    } else {
                        let initialized = ir_builder.alloc_bit(1, false);
                        ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(1u8)));
                        initialized
                    };
                    args.push(initialized);
                    args.extend(cached.elements.iter().copied());
                } else if let Some(view) = view.filter(|view| !view.elements.is_empty()) {
                    let initialized = if let Some(initialized) = view.initialized {
                        initialized
                    } else {
                        let initialized = ir_builder.alloc_bit(1, false);
                        ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(1u8)));
                        initialized
                    };
                    let Some(elements) = self.array_literal_item_elements_from_view(
                        params.key.frame,
                        params.key.var_id,
                        &cached_params.cache_key,
                        &view.elements,
                        cached_params.elements.len(),
                    ) else {
                        unreachable!("a materialized literal view covers every cached item");
                    };
                    args.push(initialized);
                    args.extend(elements);
                } else {
                    let initialized = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(0u8)));
                    args.push(initialized);
                    let layout = self.array_view_layout(params.key.var_id)?;
                    for _ in &cached_params.elements {
                        let element = if layout.is_2state {
                            ir_builder.alloc_bit(layout.element_width, layout.signed)
                        } else {
                            ir_builder.alloc_logic(layout.element_width)
                        };
                        ir_builder.emit(SIRInstruction::Imm(element, SIRValue::new(0u8)));
                        args.push(element);
                    }
                }
            }
        }
        Ok(args)
    }

    fn install_merged_array_views(
        &mut self,
        params: &[ArrayViewMergeParams],
        states: &[&Vec<HashMap<VarId, FunctionArrayView>>],
    ) {
        for params in params {
            let views = states
                .iter()
                .filter_map(|state| state[params.key.frame].get(&params.key.var_id))
                .collect::<Vec<_>>();
            let Some(first) = views.first() else {
                continue;
            };
            if views.iter().any(|view| {
                view.backing_var_id != first.backing_var_id
                    || view.owns_backing != first.owns_backing
            }) {
                unreachable!("a function array view has stable backing metadata");
            }
            let cached_literal_items = params
                .cached_literal_items
                .iter()
                .map(|cached| {
                    (
                        cached.cache_key.clone(),
                        FunctionArrayLiteralItemCache {
                            elements: cached.elements.clone(),
                            initialized: Some(cached.initialized),
                        },
                    )
                })
                .collect();
            self.function_array_view_stack[params.key.frame].insert(
                params.key.var_id,
                FunctionArrayView {
                    backing_var_id: first.backing_var_id,
                    elements: params.elements.clone(),
                    owns_backing: first.owns_backing,
                    cached_literal_items,
                    initialized: Some(params.initialized),
                },
            );
        }
    }

    pub(super) fn restore_active_function_array_views<A>(
        &self,
        finished_views: &HashMap<VarId, FunctionArrayView>,
        convert: &impl Fn(VarId, u32) -> A,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let clobbered = finished_views
            .values()
            .filter(|view| view.owns_backing && !view.elements.is_empty())
            .map(|view| view.backing_var_id)
            .collect::<HashSet<_>>();
        let active_owned_backings = self
            .function_array_view_stack
            .iter()
            .flat_map(|frame| frame.values())
            .filter(|view| view.owns_backing && !view.elements.is_empty())
            .map(|view| view.backing_var_id)
            .collect::<HashSet<_>>();
        // An owning snapshot is authoritative; restoring an alias afterward
        // could overwrite it with a duplicate conditional placeholder.
        // Older callers must be restored first so that the nearest active
        // invocation is the final snapshot left in the shared formal region.
        for frame in &self.function_array_view_stack {
            for view in frame.values() {
                if !clobbered.contains(&view.backing_var_id)
                    || view.elements.is_empty()
                    || (!view.owns_backing && active_owned_backings.contains(&view.backing_var_id))
                {
                    continue;
                }
                let layout = self.array_view_layout(view.backing_var_id)?;
                if let Some(initialized) = view.initialized {
                    let restore_block = ir_builder.new_block();
                    let merge_block = ir_builder.new_block();
                    ir_builder.seal_block(SIRTerminator::Branch {
                        cond: initialized,
                        true_block: (restore_block, vec![]),
                        false_block: (merge_block, vec![]),
                    });
                    ir_builder.switch_to_block(restore_block);
                    self.store_array_view_elements(
                        view.backing_var_id,
                        &view.elements,
                        layout.element_width,
                        convert,
                        ir_builder,
                    );
                    ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
                    ir_builder.switch_to_block(merge_block);
                } else {
                    self.store_array_view_elements(
                        view.backing_var_id,
                        &view.elements,
                        layout.element_width,
                        convert,
                        ir_builder,
                    );
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn materialize_bound_array_literal_access<A>(
        &mut self,
        var_id: VarId,
        items: &[ArrayLiteralItem],
        index: &VarIndex,
        select: &VarSelect,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<bool, ParserError> {
        if self.array_access_needs_view(var_id, index, select) {
            return Ok(false);
        }
        let formal = &self.module.variables[&var_id];
        let array_dims = formal
            .r#type
            .array
            .iter()
            .copied()
            .flatten()
            .collect::<Vec<_>>();
        let all_indices = index.0.iter().chain(&select.0).collect::<Vec<_>>();
        let resolved_indices = all_indices
            .iter()
            .map(|expr| self.get_constant_value(expr).unwrap() as usize)
            .collect::<Vec<_>>();
        let Some(selection) =
            self.select_array_literal_element(items, &resolved_indices, &array_dims)
        else {
            return Ok(false);
        };
        let Some(frame) = (0..self.function_arg_stack.len())
            .rev()
            .find(|&frame| self.function_arg_stack[frame].contains_key(&var_id))
        else {
            return Ok(false);
        };
        let cached = self.function_array_view_stack[frame]
            .get(&var_id)
            .and_then(|view| view.cached_literal_items.get(&selection.cache_key))
            .cloned();
        let item_elements = if let Some(cached) = cached {
            if let Some(initialized) = cached.initialized {
                let layout = self.array_view_layout(var_id)?;
                let array_view_candidates = self.array_view_merge_candidates([selection.expr]);
                let array_view_params =
                    self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
                let pre_array_views = self.function_array_view_stack.clone();
                let merge_elements = cached
                    .elements
                    .iter()
                    .map(|reg| ir_builder.alloc_reg(ir_builder.register(reg).clone()))
                    .collect::<Vec<_>>();
                let evaluate_block = ir_builder.new_block();
                let mut merge_params = merge_elements.clone();
                Self::append_array_view_merge_registers(&array_view_params, &mut merge_params);
                let merge_block = ir_builder.new_block_with(merge_params);
                let pre_defined = self.defined_ranges.clone();
                let pre_dynamic = self.dynamic_defined_vars.clone();
                let mut cached_args = cached.elements;
                cached_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: initialized,
                    true_block: (merge_block, cached_args),
                    false_block: (evaluate_block, vec![]),
                });
                ir_builder.switch_to_block(evaluate_block);
                let mut evaluated = self.evaluate_array_literal_item(
                    selection.expr,
                    selection.element_count,
                    layout,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                let evaluated_defined =
                    std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
                let evaluated_dynamic =
                    std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
                let evaluated_array_views = self.function_array_view_stack.clone();
                evaluated.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
                ir_builder.seal_block(SIRTerminator::Jump(merge_block, evaluated));
                ir_builder.switch_to_block(merge_block);
                self.function_array_view_stack = pre_array_views.clone();
                self.install_merged_array_views(
                    &array_view_params,
                    &[&pre_array_views, &evaluated_array_views],
                );
                self.defined_ranges = self.intersect_defined_states(pre_defined, evaluated_defined);
                self.dynamic_defined_vars =
                    self.intersect_dynamic_vars(pre_dynamic, evaluated_dynamic);
                self.function_array_view_stack[frame]
                    .get_mut(&var_id)
                    .unwrap()
                    .cached_literal_items
                    .insert(
                        selection.cache_key.clone(),
                        FunctionArrayLiteralItemCache {
                            elements: merge_elements.clone(),
                            initialized: None,
                        },
                    );
                merge_elements
            } else {
                cached.elements
            }
        } else if let Some((initialized, view_elements)) = self.function_array_view_stack[frame]
            .get(&var_id)
            .and_then(|view| {
                (!view.elements.is_empty()).then_some((view.initialized?, view.elements.clone()))
            })
        {
            let layout = self.array_view_layout(var_id)?;
            let array_view_candidates = self.array_view_merge_candidates([selection.expr]);
            let array_view_params =
                self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
            let pre_array_views = self.function_array_view_stack.clone();
            let mut carried = vec![None; selection.element_count];
            for linear_index in 0..layout.element_count {
                let mut remainder = linear_index;
                let mut coordinates = vec![0usize; array_dims.len()];
                for dimension in (0..array_dims.len()).rev() {
                    coordinates[dimension] = remainder % array_dims[dimension];
                    remainder /= array_dims[dimension];
                }
                let Some(candidate) =
                    self.select_array_literal_element(items, &coordinates, &array_dims)
                else {
                    unreachable!("validated array literal covers every formal element");
                };
                if candidate.cache_key == selection.cache_key {
                    carried[candidate.element_index] = Some(view_elements[linear_index]);
                }
            }
            let carried = carried
                .into_iter()
                .map(|element| {
                    element.unwrap_or_else(|| {
                        unreachable!("every array-valued literal item element maps to the formal")
                    })
                })
                .collect::<Vec<_>>();
            let merge_elements = carried
                .iter()
                .map(|reg| ir_builder.alloc_reg(ir_builder.register(reg).clone()))
                .collect::<Vec<_>>();
            let evaluate_block = ir_builder.new_block();
            let mut merge_params = merge_elements.clone();
            Self::append_array_view_merge_registers(&array_view_params, &mut merge_params);
            let merge_block = ir_builder.new_block_with(merge_params);
            let pre_defined = self.defined_ranges.clone();
            let pre_dynamic = self.dynamic_defined_vars.clone();
            let mut carried_args = carried;
            carried_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: initialized,
                true_block: (merge_block, carried_args),
                false_block: (evaluate_block, vec![]),
            });
            ir_builder.switch_to_block(evaluate_block);
            let mut evaluated = self.evaluate_array_literal_item(
                selection.expr,
                selection.element_count,
                layout,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            let evaluated_defined =
                std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
            let evaluated_dynamic =
                std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
            let evaluated_array_views = self.function_array_view_stack.clone();
            evaluated.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, evaluated));
            ir_builder.switch_to_block(merge_block);
            self.function_array_view_stack = pre_array_views.clone();
            self.install_merged_array_views(
                &array_view_params,
                &[&pre_array_views, &evaluated_array_views],
            );
            self.defined_ranges = self.intersect_defined_states(pre_defined, evaluated_defined);
            self.dynamic_defined_vars = self.intersect_dynamic_vars(pre_dynamic, evaluated_dynamic);
            self.function_array_view_stack[frame]
                .get_mut(&var_id)
                .unwrap()
                .cached_literal_items
                .insert(
                    selection.cache_key.clone(),
                    FunctionArrayLiteralItemCache {
                        elements: merge_elements.clone(),
                        initialized: None,
                    },
                );
            merge_elements
        } else {
            let layout = self.array_view_layout(var_id)?;
            let selected_expr = selection.expr;
            let item_elements = self.evaluate_array_literal_item(
                selected_expr,
                selection.element_count,
                layout,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            if let Some(view) = self.function_array_view_stack[frame].get_mut(&var_id) {
                view.cached_literal_items.insert(
                    selection.cache_key.clone(),
                    FunctionArrayLiteralItemCache {
                        elements: item_elements.clone(),
                        initialized: None,
                    },
                );
            } else {
                let initialized = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(0u8)));
                self.function_array_view_stack[frame].insert(
                    var_id,
                    FunctionArrayView {
                        backing_var_id: var_id,
                        elements: Vec::new(),
                        owns_backing: true,
                        cached_literal_items: HashMap::from_iter([(
                            selection.cache_key.clone(),
                            FunctionArrayLiteralItemCache {
                                elements: item_elements.clone(),
                                initialized: None,
                            },
                        )]),
                        initialized: Some(initialized),
                    },
                );
            }
            item_elements
        };
        let Some(&selected_reg) = item_elements.get(selection.element_index) else {
            unreachable!("selected array literal item index is in bounds");
        };
        self.stack.push_back(selected_reg);
        Ok(true)
    }

    fn select_array_literal_element<'b>(
        &self,
        items: &'b [ArrayLiteralItem],
        indices: &[usize],
        dims: &[usize],
    ) -> Option<ArrayLiteralSelection<'b>> {
        self.select_array_literal_element_inner(items, indices, dims, &mut Vec::new())
    }

    fn select_array_literal_element_inner<'b>(
        &self,
        items: &'b [ArrayLiteralItem],
        indices: &[usize],
        dims: &[usize],
        path: &mut Vec<usize>,
    ) -> Option<ArrayLiteralSelection<'b>> {
        // Function argument shape validation has already rejected unresolved
        // repeats and duplicate defaults, including in nested literals.
        let (&target_idx, rest_indices) = indices.split_first()?;
        let (_dim, rest_dims) = dims.split_first()?;

        let mut pos = 0usize;
        let mut default_expr: Option<(&Expression, usize)> = None;

        for (item_index, item) in items.iter().enumerate() {
            match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    let rep_count = if let Some(rep_expr) = repeat {
                        let Some(rep_count) = self.get_constant_value(rep_expr) else {
                            unreachable!(
                                "array literal repeat must be constant after function argument shape validation"
                            );
                        };
                        rep_count as usize
                    } else {
                        1
                    };

                    if target_idx < pos + rep_count {
                        path.push(item_index);
                        if rest_dims.is_empty() {
                            return Some(ArrayLiteralSelection {
                                expr,
                                element_index: 0,
                                element_count: 1,
                                cache_key: path.clone(),
                            });
                        }
                        return match expr.as_ref() {
                            Expression::ArrayLiteral(nested, _) => self
                                .select_array_literal_element_inner(
                                    nested,
                                    rest_indices,
                                    rest_dims,
                                    path,
                                ),
                            _ if expr.comptime().r#type.array.is_empty() => {
                                Some(ArrayLiteralSelection {
                                    expr,
                                    element_index: 0,
                                    element_count: 1,
                                    cache_key: path.clone(),
                                })
                            }
                            _ => Some(ArrayLiteralSelection {
                                expr,
                                element_index: Self::linear_array_index(rest_indices, rest_dims)?,
                                element_count: rest_dims.iter().product(),
                                cache_key: path.clone(),
                            }),
                        };
                    }
                    pos += rep_count;
                }
                ArrayLiteralItem::Defaul(expr) => {
                    if default_expr.is_some() {
                        unreachable!(
                            "array literal must have at most one default after function argument shape validation"
                        );
                    }
                    default_expr = Some((expr, item_index));
                }
            }
        }

        let (default_expr, default_index) = default_expr?;
        path.push(default_index);
        if rest_dims.is_empty() {
            return Some(ArrayLiteralSelection {
                expr: default_expr,
                element_index: 0,
                element_count: 1,
                cache_key: path.clone(),
            });
        }
        match default_expr {
            Expression::ArrayLiteral(nested, _) => {
                self.select_array_literal_element_inner(nested, rest_indices, rest_dims, path)
            }
            _ if default_expr.comptime().r#type.array.is_empty() => Some(ArrayLiteralSelection {
                expr: default_expr,
                element_index: 0,
                element_count: 1,
                cache_key: path.clone(),
            }),
            _ => Some(ArrayLiteralSelection {
                expr: default_expr,
                element_index: Self::linear_array_index(rest_indices, rest_dims)?,
                element_count: rest_dims.iter().product(),
                cache_key: path.clone(),
            }),
        }
    }

    fn linear_array_index(indices: &[usize], dims: &[usize]) -> Option<usize> {
        if indices.len() != dims.len() {
            return None;
        }
        indices
            .iter()
            .zip(dims)
            .try_fold(0usize, |linear, (&index, &dim)| {
                (index < dim)
                    .then(|| linear.checked_mul(dim)?.checked_add(index))
                    .flatten()
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn load_bound_array_literal_view<A>(
        &mut self,
        var_id: VarId,
        backing_var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let width = get_access_width(self.module, var_id, index, select)?;
        let formal = &self.module.variables[&var_id];
        let dest = if formal.r#type.is_2state() {
            ir_builder.alloc_bit(width, formal.r#type.signed)
        } else {
            ir_builder.alloc_logic(width)
        };
        let mut offset =
            self.emit_offset_calc(var_id, index, select, domain, convert, sources, ir_builder)?;
        if index.0.is_empty() && select.0.is_empty() && select.1.is_none() {
            let element_count = formal
                .r#type
                .array
                .iter()
                .flatten()
                .copied()
                .product::<usize>();
            if element_count == 0 || !width.is_multiple_of(element_count) {
                unreachable!("array view width is divisible by its nonzero element count");
            }
            offset = SIROffset::PackedElements {
                bit_offset: 0,
                element_width: width / element_count,
            };
        }
        ir_builder.emit(SIRInstruction::Load(
            dest,
            convert(backing_var_id, WORKING_REGION),
            offset,
            width,
        ));
        self.stack.push_back(dest);
        Ok(())
    }

    fn eval_formal_type_select(
        &self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
    ) -> Option<BitAccess> {
        let formal_type = &self.module.variables.get(&var_id)?.r#type;
        self.eval_type_select(formal_type, index, select)
    }

    pub(super) fn emit_offset_calc<A>(
        &mut self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,

        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<SIROffset, ParserError> {
        // Keep unpacked-array indexing separate from packed bit selection.
        // Backends may give array elements a physical stride different from
        // their logical packed width, so combining both here loses essential
        // source-type information.
        let (_, strides, total_width) =
            crate::bitaccess::get_dimensions_and_strides(self.module, var_id)?;
        let geometry = crate::bitaccess::select_geometry(self.module, var_id, index, select)?;
        let array_dimension_count = self.module.variables[&var_id].r#type.array.iter().count();
        let element_width = if array_dimension_count == 0 {
            total_width
        } else {
            strides[array_dimension_count - 1]
        };
        let mut static_element_index = 0u64;
        let mut dynamic_element_index = None;
        let mut static_bit_offset = 0u64;
        let mut dynamic_bit_offset = None;
        let mut dummy_targets: Vec<VarAtomBase<A>> = Vec::new();

        for (i, expr) in index.0.iter().enumerate() {
            let stride = strides[i];
            let is_unpacked = i < array_dimension_count;
            let scale = if is_unpacked {
                stride / element_width
            } else {
                stride
            };
            if let Some(c) = self.get_constant_value(expr) {
                if is_unpacked {
                    static_element_index += c * scale as u64;
                } else {
                    static_bit_offset += c * scale as u64;
                }
            } else {
                let term_reg = self.emit_arith_term(
                    expr,
                    &mut dummy_targets,
                    scale,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                if is_unpacked {
                    add_offset_term(&mut dynamic_element_index, term_reg, ir_builder);
                } else {
                    add_offset_term(&mut dynamic_bit_offset, term_reg, ir_builder);
                }
            }
        }

        let stride_offset = index.0.len();
        let is_colon_select = matches!(&select.1, Some((VarSelectOp::Colon, _)));
        let select_dim_limit = if is_colon_select {
            select.0.len().saturating_sub(1)
        } else {
            select.0.len()
        };

        let select_len = select.0.len();
        for (i, expr) in select.0.iter().enumerate() {
            if i >= select_dim_limit {
                break;
            }
            let selected_expr = if i == select_len - 1
                && let Some((op, end_expr)) = &select.1
            {
                let (_, lsb_expr) = op.eval_expr(expr, end_expr);
                lsb_expr
            } else {
                expr.clone()
            };
            let dimension = stride_offset + i;
            let stride = strides.get(dimension).copied().unwrap_or(1);
            let is_unpacked = dimension < array_dimension_count;
            let scale = if is_unpacked {
                stride / element_width
            } else {
                stride
            };
            if let Some(c) = self.get_constant_value(&selected_expr) {
                if is_unpacked {
                    static_element_index += c * scale as u64;
                } else {
                    static_bit_offset += c * scale as u64;
                }
            } else {
                let term_reg = self.emit_arith_term(
                    &selected_expr,
                    &mut dummy_targets,
                    scale,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                if is_unpacked {
                    add_offset_term(&mut dynamic_element_index, term_reg, ir_builder);
                } else {
                    add_offset_term(&mut dynamic_bit_offset, term_reg, ir_builder);
                }
            }
        }

        if let Some((VarSelectOp::Colon, range_expr)) = &select.1 {
            let dimension = stride_offset + select_dim_limit;
            let stride = strides.get(dimension).copied().unwrap_or(1);
            let is_unpacked = dimension < array_dimension_count;
            let scale = if is_unpacked {
                stride / element_width
            } else {
                stride
            };
            if let Some(lsb_val) = crate::bitaccess::eval_constexpr(range_expr)
                .map(|v| v.to_u64_digits().first().copied().unwrap_or(0))
            {
                if is_unpacked {
                    static_element_index += lsb_val * scale as u64;
                } else {
                    static_bit_offset += lsb_val * scale as u64;
                }
            }
        }

        let selected_width = geometry.selected_width;
        let element_range_is_valid = array_dimension_count != 0
            && geometry.dimension_count >= array_dimension_count
            && static_bit_offset as usize + selected_width <= element_width;
        if dynamic_element_index.is_some() {
            add_offset_constant(&mut dynamic_element_index, static_element_index, ir_builder);
        }
        if element_range_is_valid
            && (dynamic_element_index.is_some() || dynamic_bit_offset.is_some())
        {
            let element_index = if let Some(element_index) = dynamic_element_index {
                element_index
            } else {
                let element_index = ir_builder.alloc_bit(64, false);
                ir_builder.emit(SIRInstruction::Imm(
                    element_index,
                    SIRValue::new(static_element_index),
                ));
                element_index
            };
            return Ok(SIROffset::Element {
                index: element_index,
                element_width,
                bit_offset: static_bit_offset as usize,
                dynamic_bit_offset,
            });
        }

        if let Some(element_index) = dynamic_element_index {
            let logical_element_offset = scale_offset(element_index, element_width, ir_builder);
            add_offset_term(&mut dynamic_bit_offset, logical_element_offset, ir_builder);
        }

        if dynamic_bit_offset.is_some() {
            let static_logical_offset = if dynamic_element_index.is_some() {
                static_bit_offset
            } else {
                static_element_index
                    .saturating_mul(element_width as u64)
                    .saturating_add(static_bit_offset)
            };
            add_offset_constant(&mut dynamic_bit_offset, static_logical_offset, ir_builder);
        }

        Ok(match dynamic_bit_offset {
            Some(offset) => SIROffset::Dynamic(offset),
            None => SIROffset::Static(
                (static_element_index as usize)
                    .saturating_mul(element_width)
                    .saturating_add(static_bit_offset as usize),
            ),
        })
    }

    /// Helper: returns (expr * stride)
    pub(super) fn emit_arith_term<A>(
        &mut self,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        stride: usize,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
        let idx_reg = self.stack.pop_back().unwrap();

        // Optimization possible by skipping multiplication if stride == 1
        if stride == 1 {
            Ok(idx_reg)
        } else {
            let s_reg = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Imm(s_reg, SIRValue::new(stride as u64)));

            let m_reg = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Binary(m_reg, idx_reg, BinaryOp::Mul, s_reg));
            Ok(m_reg)
        }
    }

    pub(super) fn op_load<A>(
        &mut self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let is_local_let = {
            let variable = &self.module.variables[&var_id];
            variable.affiliation == Affiliation::AlwaysFf && variable.kind == VarKind::Let
        };
        if is_local_let && let Some(&value) = self.local_let_values.get(&var_id) {
            let width = get_access_width(self.module, var_id, index, select)?;
            let selected = if index.0.is_empty() && select.0.is_empty() && select.1.is_none() {
                value
            } else {
                match self
                    .emit_offset_calc(var_id, index, select, domain, convert, sources, ir_builder)?
                {
                    SIROffset::Static(lsb)
                    | SIROffset::PackedElements {
                        bit_offset: lsb, ..
                    } => self.emit_register_slice(
                        value,
                        BitAccess::new(lsb, lsb + width - 1),
                        ir_builder,
                    ),
                    SIROffset::Dynamic(offset) => {
                        self.emit_register_dynamic_slice(value, offset, width, ir_builder)
                    }
                    SIROffset::Element {
                        index,
                        element_width,
                        bit_offset,
                        dynamic_bit_offset,
                    } => {
                        let mut logical = Some(scale_offset(index, element_width, ir_builder));
                        add_offset_constant(&mut logical, bit_offset as u64, ir_builder);
                        if let Some(dynamic_bit_offset) = dynamic_bit_offset {
                            add_offset_term(&mut logical, dynamic_bit_offset, ir_builder);
                        }
                        self.emit_register_dynamic_slice(
                            value,
                            logical.expect("scaled element index is present"),
                            width,
                            ir_builder,
                        )
                    }
                }
            };
            self.stack.push_back(selected);
            return Ok(());
        }

        // Use get_access_width for the actual element width (correct for dynamic indices).
        // eval_var_select returns the full-level range for dynamic indices, which is too
        // wide for Load/Store instructions.
        let width = get_access_width(self.module, var_id, index, select)?;
        let source_type = &self.module.variables[&var_id].r#type;
        let dest_reg = if source_type.is_2state() {
            ir_builder.alloc_bit(width, source_type.signed)
        } else {
            ir_builder.alloc_logic(width)
        };

        let offset =
            self.emit_offset_calc(var_id, index, select, domain, convert, sources, ir_builder)?;
        let load_region = if self.local_working_vars.contains(&var_id) {
            WORKING_REGION
        } else {
            STABLE_REGION
        };
        ir_builder.emit(SIRInstruction::Load(
            dest_reg,
            convert(var_id, load_region),
            offset,
            width,
        ));

        self.stack.push_back(dest_reg);

        // For source tracking, use the conservative range from eval_var_select
        // (covers all bits that might be read by a dynamic index).
        let access = eval_var_select(self.module, var_id, index, select)?;
        // Module state read in an always_ff block is always the pre-edge
        // value, even when an earlier statement in the same block assigned
        // that variable.  `defined_ranges` only describes pending writes; it
        // must not hide the old-state read from the shared-clock scheduler.
        let is_internal = self.local_working_vars.contains(&var_id);
        if !is_internal {
            sources.push(VarAtomBase::new(
                convert(var_id, STABLE_REGION),
                access.lsb,
                access.msb,
            ));
        }
        Ok(())
    }

    pub(super) fn op_store<A>(
        &mut self,
        dst: &AssignDestination,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,

        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let src_reg = self.stack.pop_back().expect("invalid ir");

        let is_direct_local_let = {
            let variable = &self.module.variables[&dst.id];
            variable.affiliation == Affiliation::AlwaysFf
                && variable.kind == VarKind::Let
                && dst.index.0.is_empty()
                && dst.select.0.is_empty()
                && dst.select.1.is_none()
        };
        // Use get_access_width for actual element width (correct for dynamic array indices).
        let target_width = get_access_width(self.module, dst.id, &dst.index, &dst.select)?;
        let access = eval_var_select(self.module, dst.id, &dst.index, &dst.select)?;
        let target_type = &self.module.variables[&dst.id].r#type;
        let src_reg = if target_type.is_2state()
            && matches!(ir_builder.register(&src_reg), RegisterType::Logic { .. })
        {
            let converted = ir_builder.alloc_bit(target_width, target_type.signed);
            ir_builder.emit(SIRInstruction::Unary(
                converted,
                UnaryOp::ToTwoState,
                src_reg,
            ));
            converted
        } else {
            src_reg
        };
        if is_direct_local_let {
            self.local_let_values.insert(dst.id, src_reg);
            return Ok(());
        }

        let mut offset = self.emit_offset_calc(
            dst.id,
            &dst.index,
            &dst.select,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        if !target_type.array.is_empty()
            && dst.index.0.is_empty()
            && dst.select.0.is_empty()
            && dst.select.1.is_none()
        {
            let Some(element_count) = target_type
                .array
                .iter()
                .copied()
                .try_fold(1usize, |total, dim| {
                    dim.and_then(|dim| total.checked_mul(dim))
                })
            else {
                unreachable!("whole-array store dimensions are resolved without overflow");
            };
            if element_count == 0 || !target_width.is_multiple_of(element_count) {
                unreachable!("whole-array store width is divisible by its nonzero element count");
            }
            offset = SIROffset::PackedElements {
                bit_offset: 0,
                element_width: target_width / element_count,
            };
        }
        let is_static = is_static_access(&dst.index, &dst.select);
        let store_region = if matches!(domain, Domain::Ff)
            && (!is_static || self.sparse_write_vars.contains(&dst.id))
        {
            SPARSE_WORKING_REGION
        } else {
            domain.region()
        };
        let direct_write = if is_static {
            self.direct_static_write_ranges
                .get(&dst.id)
                .is_some_and(|ranges| {
                    ranges
                        .iter()
                        .any(|range| range.lsb <= access.lsb && range.msb >= access.msb)
                })
        } else {
            self.direct_dynamic_write_vars.contains(&dst.id)
        };
        let store_region =
            if direct_write && matches!(store_region, WORKING_REGION | SPARSE_WORKING_REGION) {
                STABLE_REGION
            } else {
                store_region
            };
        ir_builder.emit(SIRInstruction::Store(
            convert(dst.id, store_region),
            offset,
            target_width,
            src_reg,
            Vec::new(),
            Vec::new(),
        ));

        // Use conservative range from eval_var_select for tracking (covers all possible bits).
        if is_static {
            let bits = self.defined_ranges.entry(dst.id).or_default();
            for i in access.lsb..=access.msb {
                bits.insert(i);
            }
        } else {
            self.dynamic_write_vars.insert(dst.id);
        }
        self.dynamic_defined_vars.insert(dst.id);

        if matches!(domain, Domain::Ff) {
            // This is a temporary hack since we don't know the clock yet.
            // We will move targets into clock-specific buckets in parse_ff_declaration.
            targets.push(VarAtomBase::new(
                convert(dst.id, WORKING_REGION),
                access.lsb,
                access.msb,
            ));
        }
        Ok(())
    }

    pub(super) fn op_binary<A>(
        &mut self,
        op: &Op,
        width: usize,
        left_source_signed: bool,
        right_source_signed: bool,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let right = self.stack.pop_back().expect("invalid ir");
        let left = self.stack.pop_back().expect("invalid ir");

        // Decompose BitXnor/BitNand/BitNor into existing operations
        match op {
            Op::BitXnor => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(tmp, left, BinaryOp::Xor, right));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::BitNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            Op::BitNand => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(tmp, left, BinaryOp::And, right));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::BitNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            Op::BitNor => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(tmp, left, BinaryOp::Or, right));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::BitNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            _ => {}
        }

        let dest_reg = ir_builder.alloc_logic(width);
        let op = resolve_binary_op(*op, left_source_signed, right_source_signed);
        ir_builder.emit(SIRInstruction::Binary(dest_reg, left, op, right));
        self.stack.push_back(dest_reg);
    }

    fn parse_logic_op<A>(
        &mut self,
        is_and: bool,
        left: &Expression,
        right: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        self.parse_expression_in_context(
            left, targets, domain, convert, sources, ir_builder, None,
        )?;
        let lhs = self.stack.pop_back().unwrap();
        let array_view_candidates = self.array_view_merge_candidates([right]);
        let array_view_params =
            self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
        let pre_rhs_array_views = self.function_array_view_stack.clone();
        let pre_rhs_state =
            (expression_has_side_effect(right) || !array_view_candidates.is_empty()).then(|| {
                (
                    self.defined_ranges.clone(),
                    self.dynamic_defined_vars.clone(),
                )
            });

        // Only a definite dominant value may short-circuit.  Logical-not
        // produces a one-bit 4-state truth value; ToTwoState maps its X result
        // to zero so an indeterminate LHS continues into the full operation.
        let not_lhs = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Unary(not_lhs, UnaryOp::LogicNot, lhs));
        let shortcut_truth = if is_and {
            not_lhs
        } else {
            let truth = ir_builder.alloc_logic(1);
            ir_builder.emit(SIRInstruction::Unary(truth, UnaryOp::LogicNot, not_lhs));
            truth
        };
        let shortcut = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Unary(
            shortcut,
            UnaryOp::ToTwoState,
            shortcut_truth,
        ));

        let rhs_block = ir_builder.new_block();
        let result_param = ir_builder.alloc_logic(1);
        let mut merge_params = vec![result_param];
        for params in &array_view_params {
            merge_params.push(params.initialized);
            merge_params.extend(params.elements.iter().copied());
            for cached in &params.cached_literal_items {
                merge_params.push(cached.initialized);
                merge_params.extend(cached.elements.iter().copied());
            }
        }
        let merge_block = ir_builder.new_block_with(merge_params);
        let shortcut_value = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(
            shortcut_value,
            SIRValue::new(if is_and { 0u8 } else { 1u8 }),
        ));
        let mut shortcut_args = vec![shortcut_value];
        shortcut_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: shortcut,
            true_block: (merge_block, shortcut_args),
            false_block: (rhs_block, vec![]),
        });

        ir_builder.switch_to_block(rhs_block);
        self.parse_expression_in_context(
            right, targets, domain, convert, sources, ir_builder, None,
        )?;
        let rhs = self.stack.pop_back().unwrap();
        let rhs_state = pre_rhs_state.as_ref().map(|(pre_defined, pre_dynamic)| {
            (
                std::mem::replace(&mut self.defined_ranges, pre_defined.clone()),
                std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone()),
            )
        });
        let evaluated = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Binary(
            evaluated,
            lhs,
            if is_and {
                BinaryOp::LogicAnd
            } else {
                BinaryOp::LogicOr
            },
            rhs,
        ));
        let rhs_array_views = self.function_array_view_stack.clone();
        let mut rhs_args = vec![evaluated];
        rhs_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
        ir_builder.seal_block(SIRTerminator::Jump(merge_block, rhs_args));

        ir_builder.switch_to_block(merge_block);
        self.function_array_view_stack = pre_rhs_array_views.clone();
        self.install_merged_array_views(
            &array_view_params,
            &[&pre_rhs_array_views, &rhs_array_views],
        );
        if let (Some((pre_defined, pre_dynamic)), Some((rhs_defined, rhs_dynamic))) =
            (pre_rhs_state, rhs_state)
        {
            self.defined_ranges = self.intersect_defined_states(pre_defined, rhs_defined);
            self.dynamic_defined_vars = self.intersect_dynamic_vars(pre_dynamic, rhs_dynamic);
        }
        self.stack.push_back(result_param);
        Ok(())
    }

    pub(super) fn op_unary<A>(&mut self, op: &Op, width: usize, ir_builder: &mut SIRBuilder<A>) {
        let expr = self.stack.pop_back().expect("invalid ir");

        // Decompose Reduction Nand/Nor/Xnor into existing reduction + Not
        match op {
            Op::BitNand => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(tmp, UnaryOp::And, expr));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::LogicNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            Op::BitNor => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(tmp, UnaryOp::Or, expr));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::LogicNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            Op::BitXnor => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(tmp, UnaryOp::Xor, expr));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::LogicNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            _ => {}
        }

        let dest_reg = ir_builder.alloc_logic(width);
        let op = match op {
            Op::Pow => unreachable!("Pow is binary and must not be lowered by op_unary"),
            Op::Div => unreachable!("Div is binary and must not be lowered by op_unary"),
            Op::Rem => unreachable!("Rem is binary and must not be lowered by op_unary"),
            Op::Mul => unreachable!("Mul is binary and must not be lowered by op_unary"),
            Op::Add => UnaryOp::Ident,
            Op::Sub => UnaryOp::Minus,
            Op::ArithShiftL => {
                unreachable!("ArithShiftL is binary and must not be lowered by op_unary")
            }
            Op::ArithShiftR => {
                unreachable!("ArithShiftR is binary and must not be lowered by op_unary")
            }
            Op::LogicShiftL => {
                unreachable!("LogicShiftL is binary and must not be lowered by op_unary")
            }
            Op::LogicShiftR => {
                unreachable!("LogicShiftR is binary and must not be lowered by op_unary")
            }
            Op::LessEq => unreachable!("LessEq is binary and must not be lowered by op_unary"),
            Op::GreaterEq => {
                unreachable!("GreaterEq is binary and must not be lowered by op_unary")
            }
            Op::Less => unreachable!("Less is binary and must not be lowered by op_unary"),
            Op::Greater => unreachable!("Greater is binary and must not be lowered by op_unary"),
            Op::Eq => unreachable!("Eq is binary and must not be lowered by op_unary"),
            Op::EqWildcard => {
                unreachable!("EqWildcard is binary and must not be lowered by op_unary")
            }
            Op::Ne => unreachable!("Ne is binary and must not be lowered by op_unary"),
            Op::NeWildcard => {
                unreachable!("NeWildcard is binary and must not be lowered by op_unary")
            }
            Op::LogicAnd => {
                unreachable!("LogicAnd is binary and must not be lowered by op_unary")
            }
            Op::LogicOr => unreachable!("LogicOr is binary and must not be lowered by op_unary"),
            Op::LogicNot => UnaryOp::LogicNot,
            Op::BitAnd => UnaryOp::And,
            Op::BitOr => UnaryOp::Or,
            Op::BitXor => UnaryOp::Xor,
            // BitNand, BitNor, BitXnor are handled above via decomposition
            Op::BitNand | Op::BitNor | Op::BitXnor => unreachable!(),
            Op::BitNot => UnaryOp::BitNot,
            Op::As => unreachable!("As is binary and must not be lowered by op_unary"),
            Op::Ternary => {
                unreachable!("Ternary expression must be lowered by ternary-specific path")
            }
            Op::Concatenation => {
                unreachable!("Concatenation must be lowered by concat-specific path")
            }
            Op::ArrayLiteral => unreachable!("Array literal must not be lowered by op_unary"),
            Op::Condition => unreachable!("Condition node must not be lowered by op_unary"),
            Op::Repeat => unreachable!("Repeat node must be lowered by repeat-specific path"),
        };
        ir_builder.emit(SIRInstruction::Unary(dest_reg, op, expr));
        self.stack.push_back(dest_reg);
    }

    pub(super) fn emit_multi_dst_assign<A>(
        &mut self,
        rhs_reg: RegisterId,
        dsts: &[AssignDestination],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let mut current_offset = 0;
        let rhs_width = ir_builder.register(&rhs_reg).width();

        for dst in dsts.iter().rev() {
            let part_width = get_access_width(self.module, dst.id, &dst.index, &dst.select)?;

            let final_reg = if current_offset == 0 && part_width == rhs_width {
                rhs_reg
            } else {
                let shifted_reg = if current_offset == 0 {
                    rhs_reg
                } else {
                    let shifted_reg = ir_builder.alloc_logic(rhs_width);

                    let shift_amt_reg = ir_builder.alloc_bit(64, false);
                    ir_builder.emit(SIRInstruction::Imm(
                        shift_amt_reg,
                        SIRValue::new(current_offset),
                    ));

                    ir_builder.emit(SIRInstruction::Binary(
                        shifted_reg,
                        rhs_reg,
                        BinaryOp::Shr,
                        shift_amt_reg,
                    ));
                    shifted_reg
                };

                if part_width == rhs_width && current_offset == 0 {
                    shifted_reg
                } else {
                    let mask_val = (BigUint::from(1u64) << part_width) - BigUint::from(1u64);
                    let mask_reg = ir_builder.alloc_bit(part_width, false);
                    ir_builder.emit(SIRInstruction::Imm(mask_reg, SIRValue::new(mask_val)));

                    let final_reg = ir_builder.alloc_logic(part_width);
                    ir_builder.emit(SIRInstruction::Binary(
                        final_reg,
                        shifted_reg,
                        BinaryOp::And,
                        mask_reg,
                    ));
                    final_reg
                }
            };

            self.stack.push_back(final_reg);
            self.op_store(dst, targets, domain, convert, sources, ir_builder)?;

            current_offset += part_width;
        }
        Ok(())
    }

    pub(super) fn parse_assign_statement<A>(
        &mut self,
        assign_statement: &AssignStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let expected_width: usize = assign_statement
            .dst
            .iter()
            .map(|dst| get_access_width(self.module, dst.id, &dst.index, &dst.select))
            .sum::<Result<usize, ParserError>>()?;

        self.parse_expression(
            &assign_statement.expr,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            Some(expected_width),
        )?;
        let rhs_reg = self.stack.pop_back().expect("Invalid RHS");
        self.emit_multi_dst_assign(
            rhs_reg,
            &assign_statement.dst,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )
    }

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

    fn system_function_type_bits_width(ty: &Type) -> Option<usize> {
        ty.total_width()
            .map(|width| width * ty.total_array().unwrap_or(1))
    }

    fn system_function_type_size(ty: &Type) -> Option<usize> {
        if let Some(size) = ty.array.first() {
            *size
        } else if let Some(size) = ty.width_expr().first().and_then(|expr| expr.numeric()) {
            Some(size)
        } else if let Some(size) = ty.width().first() {
            *size
        } else {
            ty.total_width()
        }
    }

    fn system_function_input_bits_width(
        &self,
        input: &veryl_analyzer::ir::SystemFunctionInput,
    ) -> usize {
        let comptime = input.0.comptime();
        match &comptime.value {
            ValueVariant::Type(ty) => Self::system_function_type_bits_width(ty).unwrap_or(0),
            _ => Self::system_function_type_bits_width(&comptime.r#type)
                .unwrap_or_else(|| self.get_expression_width(&input.0)),
        }
    }

    fn system_function_input_size(&self, input: &veryl_analyzer::ir::SystemFunctionInput) -> usize {
        let comptime = input.0.comptime();
        match &comptime.value {
            ValueVariant::Type(ty) => Self::system_function_type_size(ty).unwrap_or(0),
            _ => Self::system_function_type_size(&comptime.r#type)
                .unwrap_or_else(|| self.get_expression_width(&input.0)),
        }
    }

    fn parse_system_function_call<A>(
        &mut self,
        call: &SystemFunctionCall,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        match &call.kind {
            SystemFunctionKind::Bits(input) => {
                let width = self.system_function_input_bits_width(input);
                self.op_constant(SIRValue::new(width as u64), 32, ir_builder);
                Ok(())
            }
            SystemFunctionKind::Size(input) => {
                let size = self.system_function_input_size(input);
                self.op_constant(SIRValue::new(size as u64), 32, ir_builder);
                Ok(())
            }
            SystemFunctionKind::Clog2(input) => {
                self.parse_expression(
                    &input.0, targets, domain, convert, sources, ir_builder, None,
                )?;
                let arg = self.stack.pop_back().expect("Invalid $clog2 input");
                let width = ir_builder.register(&arg).width();

                let mut result = ir_builder.alloc_bit(32, false);
                ir_builder.emit(SIRInstruction::Imm(result, SIRValue::new(0u8)));
                for k in 1..=width {
                    let threshold = ir_builder.alloc_bit(width, false);
                    ir_builder.emit(SIRInstruction::Imm(
                        threshold,
                        SIRValue::new(BigUint::from(1u8) << (k - 1)),
                    ));
                    let cond = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Binary(cond, arg, BinaryOp::GtU, threshold));
                    let value = ir_builder.alloc_bit(32, false);
                    ir_builder.emit(SIRInstruction::Imm(value, SIRValue::new(k as u64)));
                    let next = ir_builder.alloc_logic(32);
                    ir_builder.emit(SIRInstruction::Mux(next, cond, value, result));
                    result = next;
                }
                self.stack.push_back(result);
                Ok(())
            }
            SystemFunctionKind::Onehot(input) => {
                self.parse_expression(
                    &input.0, targets, domain, convert, sources, ir_builder, None,
                )?;
                let arg = self.stack.pop_back().expect("Invalid $onehot input");
                let width = ir_builder.register(&arg).width();

                let zero = ir_builder.alloc_bit(width, false);
                ir_builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
                let one = ir_builder.alloc_bit(width, false);
                ir_builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));

                let arg_minus_one = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(
                    arg_minus_one,
                    arg,
                    BinaryOp::Sub,
                    one,
                ));

                let overlap = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(
                    overlap,
                    arg,
                    BinaryOp::And,
                    arg_minus_one,
                ));

                let non_zero = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(non_zero, arg, BinaryOp::Ne, zero));

                let no_overlap = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    no_overlap,
                    overlap,
                    BinaryOp::Eq,
                    zero,
                ));

                let result = ir_builder.alloc_logic(1);
                ir_builder.emit(SIRInstruction::Binary(
                    result,
                    non_zero,
                    BinaryOp::LogicAnd,
                    no_overlap,
                ));
                self.stack.push_back(result);
                Ok(())
            }
            SystemFunctionKind::Signed(input) | SystemFunctionKind::Unsigned(input) => {
                self.parse_expression(
                    &input.0, targets, domain, convert, sources, ir_builder, None,
                )?;
                let src = self
                    .stack
                    .pop_back()
                    .expect("Invalid $signed/$unsigned input");
                let width = ir_builder.register(&src).width();
                let signed = matches!(call.kind, SystemFunctionKind::Signed(_));
                let casted = match ir_builder.register(&src) {
                    RegisterType::Logic { .. } => ir_builder.alloc_logic(width),
                    RegisterType::Bit { .. } => ir_builder.alloc_bit(width, signed),
                };
                ir_builder.emit(SIRInstruction::Unary(casted, UnaryOp::Ident, src));
                self.stack.push_back(casted);
                Ok(())
            }
            SystemFunctionKind::Readmemh(_, _)
            | SystemFunctionKind::Display(_)
            | SystemFunctionKind::Write(_)
            | SystemFunctionKind::Assert { .. }
            | SystemFunctionKind::Finish => Err(ParserError::illegal_context(
                "system task in FF expression",
                format!("{call}"),
                Some(&call.comptime.token),
            )),
        }
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
                if let Some(frame) = (0..self.function_arg_stack.len())
                    .rev()
                    .find(|&frame| self.function_arg_stack[frame].contains_key(var_id))
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
                            return Err(ParserError::unsupported(
                                43,
                                LoweringPhase::FfLowering,
                                "function argument indexed access",
                                format!(
                                    "non-variable argument expression with dynamic indexed access: var_id={:?}",
                                    var_id
                                ),
                                Some(&factor.token_range()),
                            ));
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
                            return Err(ParserError::unsupported(
                                43,
                                LoweringPhase::FfLowering,
                                "function argument indexed access",
                                format!(
                                    "non-variable argument expression with dynamic indexed access: var_id={:?}",
                                    var_id
                                ),
                                Some(&factor.token_range()),
                            ));
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
                            return Err(ParserError::unsupported(
                                43,
                                LoweringPhase::FfLowering,
                                "function argument indexed access",
                                format!(
                                    "chained range access with dynamic indices: var_id={:?}",
                                    var_id
                                ),
                                Some(&factor.token_range()),
                            ));
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
                return Err(ParserError::unsupported(
                    67,
                    LoweringPhase::FfLowering,
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

    pub(super) fn parse_binary<A>(
        &mut self,
        op: &Op,
        left: &Expression,
        right: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context: Option<ValueContext>,
    ) -> Result<(), ParserError> {
        if matches!(op, Op::LogicAnd | Op::LogicOr) {
            self.parse_logic_op(
                matches!(op, Op::LogicAnd),
                left,
                right,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            if let Some(context) = context {
                let result = self.stack.pop_back().unwrap();
                let result =
                    self.cast_reg_width_ext(ir_builder, result, context.width, context.signed);
                self.stack.push_back(result);
            }
            return Ok(());
        }

        if matches!(op, Op::As) {
            let Some(cast) = cast_semantics(left, right) else {
                return Err(ParserError::unsupported(
                    68,
                    LoweringPhase::FfLowering,
                    "as cast target",
                    format!("{:?}", right),
                    Some(&right.token_range()),
                ));
            };
            self.parse_expression_in_context(
                left,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                Some(ValueContext {
                    width: cast.width,
                    signed: cast.source_signed,
                }),
            )?;
            let src = self
                .stack
                .pop_back()
                .expect("Invalid cast source expression");

            let casted = if cast.result_is_2state {
                ir_builder.alloc_bit(cast.width, cast.result_signed)
            } else {
                ir_builder.alloc_logic(cast.width)
            };
            let cast_op = if cast.result_is_2state && !cast.source_is_2state {
                UnaryOp::ToTwoState
            } else {
                UnaryOp::Ident
            };
            ir_builder.emit(SIRInstruction::Unary(casted, cast_op, src));
            let casted = if let Some(context) = context {
                self.cast_reg_width_ext(ir_builder, casted, context.width, context.signed)
            } else {
                casted
            };
            self.stack.push_back(casted);
            return Ok(());
        }

        let lhs_width = self.get_expression_width(left);
        let rhs_width = self.get_expression_width(right);
        let lhs_signed = expression_signed(left);
        let rhs_signed = expression_signed(right);
        let semantics =
            binary_semantics(*op, lhs_width, rhs_width, lhs_signed, rhs_signed, context);

        if matches!(op, Op::Pow) {
            let Some(exp) = self.get_constant_value(right) else {
                return Err(ParserError::unsupported(
                    68,
                    LoweringPhase::FfLowering,
                    "pow non-constant exponent",
                    format!("{:?}", right),
                    Some(&right.token_range()),
                ));
            };

            let width = semantics.result_width;
            self.parse_expression_in_context(
                left,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                semantics.lhs_context,
            )?;
            let base = self
                .stack
                .pop_back()
                .expect("Invalid LHS for power operation");

            let result = if exp == 0 {
                let one = ir_builder.alloc_bit(width, false);
                ir_builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u32)));
                one
            } else {
                let mut acc = base;
                for _ in 1..exp {
                    let next = ir_builder.alloc_logic(width);
                    ir_builder.emit(SIRInstruction::Binary(next, acc, BinaryOp::Mul, base));
                    acc = next;
                }
                acc
            };

            let result = if let Some(context) = context {
                self.cast_reg_width_ext(ir_builder, result, context.width, context.signed)
            } else {
                result
            };
            self.stack.push_back(result);
            return Ok(());
        }

        self.parse_expression_in_context(
            left,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            semantics.lhs_context,
        )?;
        self.parse_expression_in_context(
            right,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            semantics.rhs_context,
        )?;
        self.op_binary(
            op,
            semantics.result_width,
            semantics.lhs_signed,
            semantics.rhs_signed,
            ir_builder,
        );
        if let Some(context) = context {
            let result = self.stack.pop_back().unwrap();
            let result = self.cast_reg_width_ext(ir_builder, result, context.width, context.signed);
            self.stack.push_back(result);
        }
        Ok(())
    }

    pub(super) fn parse_unary<A>(
        &mut self,
        op: &Op,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context: Option<ValueContext>,
    ) -> Result<(), ParserError> {
        let is_reduction = matches!(
            op,
            Op::BitAnd
                | Op::BitOr
                | Op::BitXor
                | Op::BitNand
                | Op::BitNor
                | Op::BitXnor
                | Op::LogicNot
        );
        let width = if is_reduction {
            1
        } else {
            self.get_expression_width(expr)
                .max(context.map(|context| context.width).unwrap_or(0))
        };
        // Reduction and logical-not operators reduce a multi-bit operand to 1 bit.
        // The operand must be evaluated at its own natural width, not the (narrower)
        // context width of the result — otherwise the input gets truncated before
        // the reduction is applied.
        let operand_context = if is_reduction { None } else { context };
        self.parse_expression_in_context(
            expr,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            operand_context,
        )?;
        self.op_unary(op, width, ir_builder);
        if is_reduction && let Some(context) = context {
            let result = self.stack.pop_back().unwrap();
            let result = self.cast_reg_width_ext(ir_builder, result, context.width, context.signed);
            self.stack.push_back(result);
        }
        Ok(())
    }

    pub(super) fn parse_ternary<A>(
        &mut self,
        cond: &Expression,
        then: &Expression,
        els: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context: Option<ValueContext>,
    ) -> Result<(), ParserError> {
        let branch_context = ValueContext {
            width: self
                .get_expression_width(then)
                .max(self.get_expression_width(els))
                .max(context.map(|context| context.width).unwrap_or(0)),
            signed: context
                .map(|context| context.signed)
                .unwrap_or_else(|| expression_signed(then) && expression_signed(els)),
        };
        let result_width = branch_context.width;
        self.parse_expression_in_context(
            cond, targets, domain, convert, sources, ir_builder, None,
        )?;
        let cond_reg = self.stack.pop_back().unwrap();

        let array_view_candidates = self.array_view_merge_candidates([then, els]);
        let branch_materializes_array_view = !array_view_candidates.is_empty();

        if !expression_has_side_effect(then)
            && !expression_has_side_effect(els)
            && !branch_materializes_array_view
        {
            self.parse_expression_in_context(
                then,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                Some(branch_context),
            )?;
            let then_val = self.stack.pop_back().unwrap();
            self.parse_expression_in_context(
                els,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                Some(branch_context),
            )?;
            let else_val = self.stack.pop_back().unwrap();
            let result = ir_builder.alloc_logic(result_width);
            ir_builder.emit(SIRInstruction::Mux(result, cond_reg, then_val, else_val));
            self.stack.push_back(result);
            return Ok(());
        }

        let pre_ternary_defined = self.defined_ranges.clone();
        let pre_ternary_dynamic = self.dynamic_defined_vars.clone();
        let pre_ternary_array_views = self.function_array_view_stack.clone();

        // A known condition evaluates only the selected arm. An X/Z
        // condition evaluates both arms and merges their bits.
        let not_cond = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Unary(not_cond, UnaryOp::LogicNot, cond_reg));
        let known_false = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Unary(
            known_false,
            UnaryOp::ToTwoState,
            not_cond,
        ));
        let true_truth = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Unary(
            true_truth,
            UnaryOp::LogicNot,
            not_cond,
        ));
        let known_true = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Unary(
            known_true,
            UnaryOp::ToTwoState,
            true_truth,
        ));

        let dummy_then = ir_builder.alloc_logic(result_width);
        ir_builder.emit(SIRInstruction::Imm(dummy_then, SIRValue::new(0u8)));
        let direct_else = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(direct_else, SIRValue::new(0u8)));
        let merge_else = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(merge_else, SIRValue::new(1u8)));

        let else_view_params =
            self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
        let merge_view_params =
            self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
        let then_block = ir_builder.new_block();
        let carried_then = ir_builder.alloc_logic(result_width);
        let needs_merge = ir_builder.alloc_bit(1, false);
        let mut else_params = vec![carried_then, needs_merge];
        for params in &else_view_params {
            else_params.push(params.initialized);
            else_params.extend(params.elements.iter().copied());
            for cached in &params.cached_literal_items {
                else_params.push(cached.initialized);
                else_params.extend(cached.elements.iter().copied());
            }
        }
        let else_block = ir_builder.new_block_with(else_params);
        let result = ir_builder.alloc_logic(result_width);
        let mut merge_params = vec![result];
        for params in &merge_view_params {
            merge_params.push(params.initialized);
            merge_params.extend(params.elements.iter().copied());
            for cached in &params.cached_literal_items {
                merge_params.push(cached.initialized);
                merge_params.extend(cached.elements.iter().copied());
            }
        }
        let merge_block = ir_builder.new_block_with(merge_params);

        let mut initial_else_args = vec![dummy_then, direct_else];
        initial_else_args.extend(self.array_view_state_args(&else_view_params, ir_builder)?);
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: known_false,
            true_block: (else_block, initial_else_args),
            false_block: (then_block, vec![]),
        });

        ir_builder.switch_to_block(then_block);
        self.parse_expression_in_context(
            then,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            Some(branch_context),
        )?;
        let then_val = self.stack.pop_back().unwrap();
        let then_defined = std::mem::replace(&mut self.defined_ranges, pre_ternary_defined.clone());
        let then_dynamic =
            std::mem::replace(&mut self.dynamic_defined_vars, pre_ternary_dynamic.clone());
        let then_array_views = self.function_array_view_stack.clone();
        let mut then_merge_args = vec![then_val];
        let mut then_else_args = vec![then_val, merge_else];
        then_merge_args.extend(self.array_view_state_args(&merge_view_params, ir_builder)?);
        then_else_args.extend(self.array_view_state_args(&else_view_params, ir_builder)?);
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: known_true,
            true_block: (merge_block, then_merge_args),
            false_block: (else_block, then_else_args),
        });

        self.function_array_view_stack = pre_ternary_array_views.clone();
        ir_builder.switch_to_block(else_block);
        self.install_merged_array_views(
            &else_view_params,
            &[&pre_ternary_array_views, &then_array_views],
        );
        self.parse_expression_in_context(
            els,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            Some(branch_context),
        )?;
        let else_val = self.stack.pop_back().unwrap();
        let else_defined = std::mem::take(&mut self.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.dynamic_defined_vars);
        let else_array_views = self.function_array_view_stack.clone();
        let merged = ir_builder.alloc_logic(result_width);
        ir_builder.emit(SIRInstruction::Mux(
            merged,
            cond_reg,
            carried_then,
            else_val,
        ));
        let direct_else_block = ir_builder.new_block();
        let mut else_merge_state = Vec::new();
        else_merge_state.extend(self.array_view_state_args(&merge_view_params, ir_builder)?);
        let mut merged_args = vec![merged];
        merged_args.extend(else_merge_state.iter().copied());
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: needs_merge,
            true_block: (merge_block, merged_args),
            false_block: (direct_else_block, vec![]),
        });

        ir_builder.switch_to_block(direct_else_block);
        let mut direct_else_args = vec![else_val];
        direct_else_args.extend(else_merge_state);
        ir_builder.seal_block(SIRTerminator::Jump(merge_block, direct_else_args));

        self.function_array_view_stack = pre_ternary_array_views;

        ir_builder.switch_to_block(merge_block);
        let pre_views = self.function_array_view_stack.clone();
        self.install_merged_array_views(
            &merge_view_params,
            &[&pre_views, &then_array_views, &else_array_views],
        );
        self.defined_ranges = self.intersect_defined_states(then_defined, else_defined);
        self.dynamic_defined_vars = self.intersect_dynamic_vars(then_dynamic, else_dynamic);
        self.stack.push_back(result);
        Ok(())
    }

    pub(super) fn parse_concatenation<A>(
        &mut self,
        exprs: &[(Expression, Option<Expression>)],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,

        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let mut total_width = 0;

        // Create accumulator with initial value 0
        let mut acc_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(acc_reg, SIRValue::new(0u32)));

        // Parse sequentially from right (LSB)
        for (expr, replication) in exprs.iter().rev() {
            // 1. Evaluate expression to be repeated
            self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
            let part_reg = self
                .stack
                .pop_back()
                .expect("Concatenation part evaluation failed");
            let part_width = ir_builder.register(&part_reg).width();

            // 2. Get replication count (1 if not specified)
            let rep_count = if let Some(rep_expr) = replication {
                use crate::bitaccess::eval_constexpr;
                let v = eval_constexpr(rep_expr);
                v.unwrap().iter_u64_digits().next().unwrap()
            } else {
                1
            };

            // 3. Repeat packing for the specified number of times
            for _ in 0..rep_count {
                let next_total_width = total_width + part_width;

                // Generate left shift amount

                let shift_amt_reg = ir_builder.alloc_bit(64, false);
                ir_builder.emit(SIRInstruction::Imm(
                    shift_amt_reg,
                    SIRValue::new(total_width),
                ));

                // Shift target to current position
                let shifted_part_reg = ir_builder.alloc_logic(next_total_width);
                ir_builder.emit(SIRInstruction::Binary(
                    shifted_part_reg,
                    part_reg,
                    BinaryOp::Shl,
                    shift_amt_reg,
                ));

                // Integrate into accumulator
                let next_acc_reg = ir_builder.alloc_logic(next_total_width);
                ir_builder.emit(SIRInstruction::Binary(
                    next_acc_reg,
                    acc_reg,
                    BinaryOp::Or,
                    shifted_part_reg,
                ));

                // Update state
                acc_reg = next_acc_reg;
                total_width = next_total_width;
            }
        }

        // Push final result to stack
        self.stack.push_back(acc_reg);
        Ok(())
    }

    pub(super) fn emit_concat_registers<A>(
        &mut self,
        parts: &[(RegisterId, usize)],
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        if parts.is_empty() {
            let reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Imm(reg, SIRValue::new(0u32)));
            return reg;
        }
        if parts.len() == 1 {
            return parts[0].0;
        }

        let mut total_width = 0usize;
        let mut acc_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(acc_reg, SIRValue::new(0u32)));

        for (part_reg, part_width) in parts.iter().rev() {
            let next_total_width = total_width + *part_width;

            let shift_amt_reg = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Imm(
                shift_amt_reg,
                SIRValue::new(total_width),
            ));

            let shifted_part_reg = ir_builder.alloc_logic(next_total_width);
            ir_builder.emit(SIRInstruction::Binary(
                shifted_part_reg,
                *part_reg,
                BinaryOp::Shl,
                shift_amt_reg,
            ));

            let next_acc_reg = ir_builder.alloc_logic(next_total_width);
            ir_builder.emit(SIRInstruction::Binary(
                next_acc_reg,
                acc_reg,
                BinaryOp::Or,
                shifted_part_reg,
            ));

            acc_reg = next_acc_reg;
            total_width = next_total_width;
        }

        acc_reg
    }

    pub(super) fn parse_struct_constructor<A>(
        &mut self,
        ty: &Type,
        fields: &Vec<(veryl_parser::resource_table::StrId, Expression)>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        _context_width: Option<usize>,
    ) -> Result<(), ParserError> {
        let mut parts: Vec<(RegisterId, usize)> = Vec::new();

        for (name, expr) in fields {
            let Some(member_type) = ty.get_member_type(*name) else {
                return Err(ParserError::unsupported(
                    68,
                    LoweringPhase::FfLowering,
                    "struct constructor member",
                    format!("unknown member: {:?} in {:?}", name, ty),
                    Some(&expr.token_range()),
                ));
            };
            let Some(member_width) = member_type.total_width() else {
                return Err(ParserError::unsupported(
                    68,
                    LoweringPhase::FfLowering,
                    "struct constructor member width",
                    format!("member: {:?}, type: {:?}", name, member_type),
                    Some(&expr.token_range()),
                ));
            };
            self.parse_expression(
                expr,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                Some(member_width),
            )?;
            let mut reg = self
                .stack
                .pop_back()
                .expect("Struct constructor part evaluation failed");
            reg = self.coerce_register_to_formal(
                ir_builder,
                reg,
                member_width,
                expression_signed(expr),
                member_type.signed,
                member_type.is_2state(),
            );

            parts.push((reg, member_width));
        }

        let reg = self.emit_concat_registers(&parts, ir_builder);
        self.stack.push_back(reg);
        Ok(())
    }

    pub(super) fn parse_array_literal<A>(
        &mut self,
        items: &Vec<ArrayLiteralItem>,
        expected_width: Option<usize>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let mut parts: Vec<(RegisterId, usize)> = Vec::new();
        let mut explicit_width = 0usize;
        let mut default_part: Option<(RegisterId, usize)> = None;

        for item in items {
            match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    self.parse_expression(
                        expr, targets, domain, convert, sources, ir_builder, None,
                    )?;
                    let part_reg = self
                        .stack
                        .pop_back()
                        .expect("Array literal part evaluation failed");
                    let part_width = ir_builder.register(&part_reg).width();

                    let rep_count = if let Some(rep_expr) = repeat {
                        self.get_constant_value(rep_expr).ok_or_else(|| {
                            ParserError::illegal_context(
                                "array literal non-constant repeat",
                                format!("{:?}", rep_expr),
                                Some(&rep_expr.token_range()),
                            )
                        })?
                    } else {
                        1
                    };

                    for _ in 0..rep_count {
                        parts.push((part_reg, part_width));
                    }
                    explicit_width += part_width * rep_count as usize;
                }
                ArrayLiteralItem::Defaul(expr) => {
                    if default_part.is_some() {
                        return Err(ParserError::illegal_context(
                            "array literal multiple default",
                            format!("{:?}", items),
                            Some(&expr.token_range()),
                        ));
                    }

                    self.parse_expression(
                        expr, targets, domain, convert, sources, ir_builder, None,
                    )?;
                    let part_reg = self
                        .stack
                        .pop_back()
                        .expect("Array literal default evaluation failed");
                    let part_width = ir_builder.register(&part_reg).width();
                    default_part = Some((part_reg, part_width));
                }
            }
        }

        if let Some((default_reg, default_width)) = default_part {
            let Some(target_width) = expected_width else {
                return Err(ParserError::unsupported(
                    68,
                    LoweringPhase::FfLowering,
                    "array literal default without context width",
                    format!("{:?}", items),
                    items.first().map(|it| it.token_range()).as_ref(),
                ));
            };

            if explicit_width > target_width {
                return Err(ParserError::illegal_context(
                    "array literal width overflow",
                    format!("explicit_width={explicit_width}, target_width={target_width}"),
                    items.first().map(|it| it.token_range()).as_ref(),
                ));
            }

            let remaining = target_width - explicit_width;
            if default_width == 0 || !remaining.is_multiple_of(default_width) {
                return Err(ParserError::illegal_context(
                    "array literal default width mismatch",
                    format!(
                        "remaining={remaining}, default_width={default_width}, target_width={target_width}"
                    ),
                    items.first().map(|it| it.token_range()).as_ref(),
                ));
            }

            for _ in 0..(remaining / default_width) {
                parts.push((default_reg, default_width));
            }
        }

        let reg = self.emit_concat_registers(&parts, ir_builder);
        self.stack.push_back(reg);
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

    fn parse_expression_in_context<A>(
        &mut self,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context: Option<ValueContext>,
    ) -> Result<(), ParserError> {
        let context_width = context.map(|context| context.width);
        // Short-circuit: compile-time constant compound expression → emit constant value.
        // Unlike the SLT path (comb.rs), the SIR path requires the register width to
        // match context_width because emit_multi_dst_assign assumes rhs_width >= part_width.
        if !matches!(expr, Expression::Term(_)) {
            let ct = expr.comptime();
            if ct.is_const {
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
                self.parse_binary(
                    op, left, right, targets, domain, convert, sources, ir_builder, context,
                )?;
            }
            Expression::Unary(op, expr, _) => {
                self.parse_unary(
                    op, expr, targets, domain, convert, sources, ir_builder, context,
                )?;
            }
            Expression::Ternary(cond, then, els, _) => {
                self.parse_ternary(
                    cond, then, els, targets, domain, convert, sources, ir_builder, context,
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
mod tests {
    use super::*;
    use crate::BuildConfig;
    use veryl_analyzer::{
        Analyzer, Context, attribute_table,
        ir::{Component, Declaration, Ir},
        symbol_table,
    };
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;

    #[test]
    fn module_state_read_after_write_is_tracked_as_an_old_state_source() {
        symbol_table::clear();
        attribute_table::clear();
        let code = r#"
module Top (
    clk    : input clock,
    present: input logic,
    d      : input logic<8>,
) {
    var in_flight: logic;
    var captured: logic<8>;
    always_ff (clk) {
        in_flight = present;
        if in_flight {
            captured = d;
        }
    }
}
"#;
        let metadata = Metadata::create_default("prj").unwrap();
        let parsed = Parser::parse(code, &"").unwrap();
        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = Ir::default();
        assert!(analyzer.analyze_pass1("prj", &parsed.veryl).is_empty());
        assert!(Analyzer::analyze_post_pass1().is_empty());
        assert!(
            analyzer
                .analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir))
                .is_empty()
        );
        assert!(Analyzer::analyze_post_pass2(&ir).is_empty());

        let module = ir
            .components
            .into_iter()
            .find_map(|component| match component {
                Component::Module(module) => Some(module),
                _ => None,
            })
            .unwrap();
        let declarations = module
            .declarations
            .iter()
            .filter_map(|declaration| match declaration {
                Declaration::Ff(declaration) => Some(declaration.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let in_flight = module
            .variables
            .iter()
            .find_map(|(&id, variable)| (variable.path.to_string() == "in_flight").then_some(id))
            .unwrap();

        let mut parser = FfParser::new(&module, BuildConfig::default());
        let mut builder = SIRBuilder::new();
        let result = parser.parse_ff_group(&declarations, &mut builder).unwrap();

        assert!(result.sources.iter().any(|source| {
            source.id.var_id == in_flight
                && source.id.region == celox_design::STABLE_REGION
                && source.access == BitAccess::new(0, 0)
        }));
    }

    #[test]
    fn direct_ff_stores_are_selected_per_bit_range() {
        symbol_table::clear();
        attribute_table::clear();
        let code = r#"
module Top (
    clk: input clock,
    lo : input logic<8>,
    hi : input logic<8>,
) {
    var state: logic<16>;
    always_ff (clk) {
        state[7:0] = lo;
        state[15:8] = hi;
    }
}
"#;
        let metadata = Metadata::create_default("prj").unwrap();
        let parsed = Parser::parse(code, &"").unwrap();
        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = Ir::default();
        assert!(analyzer.analyze_pass1("prj", &parsed.veryl).is_empty());
        assert!(Analyzer::analyze_post_pass1().is_empty());
        assert!(
            analyzer
                .analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir))
                .is_empty()
        );
        assert!(Analyzer::analyze_post_pass2(&ir).is_empty());

        let module = ir
            .components
            .into_iter()
            .find_map(|component| match component {
                Component::Module(module) => Some(module),
                _ => None,
            })
            .unwrap();
        let declarations = module
            .declarations
            .iter()
            .filter_map(|declaration| match declaration {
                Declaration::Ff(declaration) => Some(declaration.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let state = module
            .variables
            .iter()
            .find_map(|(&id, variable)| (variable.path.to_string() == "state").then_some(id))
            .unwrap();
        let direct_ranges = [(state, vec![BitAccess::new(8, 15)])].into_iter().collect();

        let mut parser = FfParser::new(&module, BuildConfig::default())
            .with_direct_write_ranges(direct_ranges, HashSet::default());
        let mut builder = SIRBuilder::new();
        parser.parse_ff_group(&declarations, &mut builder).unwrap();
        let execution_unit = builder.flush_eu().unwrap();
        let stores = execution_unit
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(address, SIROffset::Static(offset), width, ..)
                    if address.var_id == state =>
                {
                    Some((address.region, *offset, *width))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(stores.contains(&(WORKING_REGION, 0, 8)));
        assert!(stores.contains(&(STABLE_REGION, 8, 8)));
    }

    #[test]
    fn conditional_expression_effects_are_not_definitely_defined_after_merge() {
        symbol_table::clear();
        attribute_table::clear();
        let code = r#"
module Top (
    clk: input clock,
    guard: input logic,
    sel: input logic,
    and_result: output logic,
    ternary_result: output logic,
    after_and: output logic,
    after_then: output logic,
    after_else: output logic,
) {
    var and_side: logic;
    var then_side: logic;
    var else_side: logic;
    function touch (value: output logic) -> logic {
        value = 1'b1;
        return 1'b1;
    }
    always_ff (clk) {
        and_result = guard && touch(and_side);
        ternary_result = if sel ? touch(then_side) : touch(else_side);
        after_and = and_side;
        after_then = then_side;
        after_else = else_side;
    }
}
"#;
        let metadata = Metadata::create_default("prj").unwrap();
        let parsed = Parser::parse(code, &"").unwrap();
        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = Ir::default();
        assert!(analyzer.analyze_pass1("prj", &parsed.veryl).is_empty());
        assert!(Analyzer::analyze_post_pass1().is_empty());
        assert!(
            analyzer
                .analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir))
                .is_empty()
        );
        assert!(Analyzer::analyze_post_pass2(&ir).is_empty());

        let module = ir
            .components
            .into_iter()
            .find_map(|component| match component {
                Component::Module(module) => Some(module),
                _ => None,
            })
            .unwrap();
        let declarations = module
            .declarations
            .iter()
            .filter_map(|declaration| match declaration {
                Declaration::Ff(declaration) => Some(declaration.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let effect_ids = module
            .variables
            .iter()
            .filter_map(|(&id, variable)| {
                matches!(
                    variable.path.to_string().as_str(),
                    "and_side" | "then_side" | "else_side"
                )
                .then_some(id)
            })
            .collect::<Vec<_>>();
        assert_eq!(effect_ids.len(), 3);

        let mut parser = FfParser::new(&module, BuildConfig::default());
        let mut builder = SIRBuilder::new();
        parser.parse_ff_group(&declarations, &mut builder).unwrap();
        builder.flush_eu().unwrap().verify();

        for id in effect_ids {
            assert!(
                parser
                    .defined_ranges
                    .get(&id)
                    .is_none_or(|bits| bits.is_empty()),
                "conditional output argument must not be definitely defined: {id:?}"
            );
            assert!(!parser.dynamic_defined_vars.contains(&id));
        }
    }
}
