use super::{Domain, FfParser};
use crate::context_width::{
    ValueContext, binary_semantics, cast_semantics, expression_signed, resolve_binary_op,
};
use crate::ir::{
    BinaryOp, BitAccess, RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIROffset,
    SIRTerminator, SIRValue, STABLE_REGION, UnaryOp, VarAtomBase, WORKING_REGION,
};
use crate::parser::{
    LoweringPhase, ParserError,
    bitaccess::{
        celox_value_from_comptime, celox_value_from_comptime_in_context, eval_var_select,
        get_access_width, is_static_access,
    },
    resolve_total_width,
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
                    crate::parser::bitaccess::eval_constexpr(e)
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
        let Some(formal) = self.module.variables.get(&var_id) else {
            return Ok(false);
        };
        if formal.r#type.array.is_empty() {
            return Ok(false);
        }
        if select.1.is_some() {
            return Ok(false);
        }

        let array_dims: Option<Vec<usize>> = formal.r#type.array.iter().copied().collect();
        let Some(array_dims) = array_dims else {
            return Ok(false);
        };

        let mut all_indices = index.0.clone();
        all_indices.extend(select.0.iter().cloned());
        if all_indices.len() != array_dims.len() {
            return Ok(false);
        }

        let mut resolved_indices = Vec::with_capacity(all_indices.len());
        for (i, expr) in all_indices.iter().enumerate() {
            let Some(idx) = self.get_constant_value(expr).map(|x| x as usize) else {
                return Ok(false);
            };
            let dim = array_dims[i];
            if idx >= dim {
                return Ok(false);
            }
            resolved_indices.push(idx);
        }

        let Some(selected_expr) =
            self.select_array_literal_element(items, &resolved_indices, &array_dims)?
        else {
            return Ok(false);
        };

        let access_width = get_access_width(self.module, var_id, index, select)?;
        self.parse_expression(
            selected_expr,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            Some(access_width),
        )?;
        let selected_reg = self.stack.pop_back().unwrap();
        let coerced = self.coerce_register_to_formal(
            ir_builder,
            selected_reg,
            access_width,
            selected_expr.comptime().r#type.signed,
            formal.r#type.signed,
            formal.r#type.is_2state(),
        );
        self.stack.push_back(coerced);
        Ok(true)
    }

    fn select_array_literal_element<'b>(
        &self,
        items: &'b [ArrayLiteralItem],
        indices: &[usize],
        dims: &[usize],
    ) -> Result<Option<&'b Expression>, ParserError> {
        let Some((&target_idx, rest_indices)) = indices.split_first() else {
            return Ok(None);
        };
        let Some((_dim, rest_dims)) = dims.split_first() else {
            return Ok(None);
        };

        let mut pos = 0usize;
        let mut default_expr: Option<&Expression> = None;

        for item in items {
            match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    let rep_count = if let Some(rep_expr) = repeat {
                        self.get_constant_value(rep_expr).ok_or_else(|| {
                            ParserError::unsupported(
                                68,
                                LoweringPhase::FfLowering,
                                "array literal non-constant repeat",
                                format!("{:?}", rep_expr),
                                Some(&rep_expr.token_range()),
                            )
                        })? as usize
                    } else {
                        1
                    };

                    if target_idx < pos + rep_count {
                        if rest_dims.is_empty() {
                            return Ok(Some(expr));
                        }
                        return match expr.as_ref() {
                            Expression::ArrayLiteral(nested, _) => {
                                self.select_array_literal_element(nested, rest_indices, rest_dims)
                            }
                            _ if expr.comptime().r#type.array.is_empty() => Ok(Some(expr)),
                            _ => Ok(None),
                        };
                    }
                    pos += rep_count;
                }
                ArrayLiteralItem::Defaul(expr) => {
                    if default_expr.is_some() {
                        return Err(ParserError::unsupported(
                            68,
                            LoweringPhase::FfLowering,
                            "array literal multiple default",
                            format!("{:?}", items),
                            Some(&expr.token_range()),
                        ));
                    }
                    default_expr = Some(expr);
                }
            }
        }

        let Some(default_expr) = default_expr else {
            return Ok(None);
        };
        if rest_dims.is_empty() {
            return Ok(Some(default_expr));
        }
        match default_expr {
            Expression::ArrayLiteral(nested, _) => {
                self.select_array_literal_element(nested, rest_indices, rest_dims)
            }
            _ if default_expr.comptime().r#type.array.is_empty() => Ok(Some(default_expr)),
            _ => Ok(None),
        }
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
        // 1. Calculate strides for all dimensions (array + width)
        let (_, strides, _) =
            crate::parser::bitaccess::get_dimensions_and_strides(self.module, var_id)?;

        // 2. Offset calculation (Static + Dynamic)
        let mut static_offset: u64 = 0;
        let mut dynamic_offset_reg: Option<RegisterId> = None;

        let mut add_dynamic_term = |term_reg: RegisterId, builder: &mut SIRBuilder<A>| {
            if let Some(curr) = dynamic_offset_reg {
                let next = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(next, curr, BinaryOp::Add, term_reg));
                dynamic_offset_reg = Some(next);
            } else {
                dynamic_offset_reg = Some(term_reg);
            }
        };

        // 3. Array index part (VarIndex)
        let mut dummy_targets: Vec<VarAtomBase<A>> = Vec::new();

        for (i, expr) in index.0.iter().enumerate() {
            let stride = strides[i];
            if let Some(c) = self.get_constant_value(expr) {
                static_offset += c * (stride as u64);
            } else {
                // Dynamic term
                let term_reg = self.emit_arith_term(
                    expr,
                    &mut dummy_targets,
                    stride,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                add_dynamic_term(term_reg, ir_builder);
            }
        }

        // 4. Bit select / Final dimension array part (VarSelect)
        // Offset stride lookup by the number of array indices already consumed
        let stride_offset = index.0.len();

        // For Colon selects (e.g. [31:0]), the last element of select.0
        // is the MSB anchor—not a dimension index.
        // Exclude it from the dynamic offset and instead handle the
        // bit range LSB separately (matching the comb path's dim_limit logic).
        let is_colon_select = matches!(&select.1, Some((VarSelectOp::Colon, _)));
        let select_dim_limit = if is_colon_select {
            select.0.len().saturating_sub(1)
        } else {
            select.0.len()
        };

        let select_len = select.0.len();
        for (i, expr) in select.0.iter().enumerate() {
            if i >= select_dim_limit {
                // This is the MSB anchor of a Colon select — skip it.
                // The LSB is handled below.
                break;
            }
            // Case: final element and slice (Option exists)
            if i == select_len - 1
                && let Some((op, end_expr)) = &select.1
            {
                // Get LSB expression using VarSelectOp::eval_expr
                let (_, lsb_expr) = op.eval_expr(expr, end_expr);

                let stride = if stride_offset + i < strides.len() {
                    strides[stride_offset + i]
                } else {
                    1
                };
                if let Some(c) = self.get_constant_value(&lsb_expr) {
                    static_offset += c * (stride as u64);
                } else {
                    let term_reg = self.emit_arith_term(
                        &lsb_expr,
                        &mut dummy_targets,
                        stride,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    add_dynamic_term(term_reg, ir_builder);
                }
            } else {
                // Normal index (array dimension or single bit select)
                let stride = if stride_offset + i < strides.len() {
                    strides[stride_offset + i]
                } else {
                    1
                };

                if let Some(c) = self.get_constant_value(expr) {
                    static_offset += c * (stride as u64);
                } else {
                    let term_reg = self.emit_arith_term(
                        expr,
                        &mut dummy_targets,
                        stride,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    add_dynamic_term(term_reg, ir_builder);
                }
            }
        }

        // For Colon selects, add the LSB as a static offset
        if let Some((VarSelectOp::Colon, range_expr)) = &select.1 {
            let weight = strides
                .get(stride_offset + select_dim_limit)
                .copied()
                .unwrap_or(1);
            if let Some(lsb_val) = crate::parser::bitaccess::eval_constexpr(range_expr)
                .map(|v| v.to_u64_digits().first().copied().unwrap_or(0))
            {
                static_offset += lsb_val * (weight as u64);
            }
        }

        if let Some(dyn_reg) = dynamic_offset_reg {
            if static_offset == 0 {
                Ok(SIROffset::Dynamic(dyn_reg))
            } else {
                // Combine dynamic + static
                let s_reg = ir_builder.alloc_bit(64, false);
                ir_builder.emit(SIRInstruction::Imm(s_reg, SIRValue::new(static_offset)));
                let total_reg = ir_builder.alloc_bit(64, false);
                ir_builder.emit(SIRInstruction::Binary(
                    total_reg,
                    dyn_reg,
                    BinaryOp::Add,
                    s_reg,
                ));
                Ok(SIROffset::Dynamic(total_reg))
            }
        } else {
            Ok(SIROffset::Static(static_offset as usize))
        }
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

    pub(super) fn is_range_fully_defined(&self, var_id: VarId, access: BitAccess) -> bool {
        if let Some(bits) = self.defined_ranges.get(&var_id) {
            // Whether all bits in the specified range [lsb, msb] are set in BitSet
            (access.lsb..=access.msb).all(|i| bits.contains(i))
        } else {
            false
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
                    SIROffset::Static(lsb) => self.emit_register_slice(
                        value,
                        BitAccess::new(lsb, lsb + width - 1),
                        ir_builder,
                    ),
                    SIROffset::Dynamic(offset) => {
                        self.emit_register_dynamic_slice(value, offset, width, ir_builder)
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
        let is_internal = self.local_working_vars.contains(&var_id)
            || self.is_range_fully_defined(var_id, access)
            || self.dynamic_defined_vars.contains(&var_id);
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

        let offset = self.emit_offset_calc(
            dst.id,
            &dst.index,
            &dst.select,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        ir_builder.emit(SIRInstruction::Store(
            convert(dst.id, domain.region()),
            offset,
            target_width,
            src_reg,
            Vec::new(),
            Vec::new(),
        ));

        // Use conservative range from eval_var_select for tracking (covers all possible bits).
        let access = eval_var_select(self.module, dst.id, &dst.index, &dst.select)?;
        let is_static = is_static_access(&dst.index, &dst.select);
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
        let pre_rhs_state = expression_has_side_effect(right).then(|| {
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
        let merge_block = ir_builder.new_block_with(vec![result_param]);
        let shortcut_value = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(
            shortcut_value,
            SIRValue::new(if is_and { 0u8 } else { 1u8 }),
        ));
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: shortcut,
            true_block: (merge_block, vec![shortcut_value]),
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
        ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![evaluated]));

        ir_builder.switch_to_block(merge_block);
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

                    if let Expression::ArrayLiteral(items, _) = &bound_expr {
                        if self.materialize_bound_array_literal_access(
                            *var_id, items, var_index, var_select, targets, domain, convert,
                            sources, ir_builder,
                        )? {
                            return Ok(());
                        }
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

                    let Factor::Variable(bound_var_id, bound_var_index, bound_var_select, _) =
                        bound_factor.as_ref()
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

                    self.op_load(
                        *bound_var_id,
                        &merged_index,
                        &merged_select,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                } else {
                    self.op_load(
                        *var_id, var_index, var_select, domain, convert, sources, ir_builder,
                    )?;
                }
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
            Factor::Anonymous(_) | Factor::Unknown(_) => {
                unreachable!("Expression factors must be resolved before FF lowering")
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
        let branch_context = Some(ValueContext {
            width: self
                .get_expression_width(then)
                .max(self.get_expression_width(els))
                .max(context.map(|context| context.width).unwrap_or(0)),
            signed: context
                .map(|context| context.signed)
                .unwrap_or_else(|| expression_signed(then) && expression_signed(els)),
        });
        let result_width = branch_context.unwrap().width;
        self.parse_expression_in_context(
            cond, targets, domain, convert, sources, ir_builder, None,
        )?;
        let cond_reg = self.stack.pop_back().unwrap();

        if !expression_has_side_effect(then) && !expression_has_side_effect(els) {
            self.parse_expression_in_context(
                then,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                branch_context,
            )?;
            let then_val = self.stack.pop_back().unwrap();
            self.parse_expression_in_context(
                els,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                branch_context,
            )?;
            let else_val = self.stack.pop_back().unwrap();
            let result = ir_builder.alloc_logic(result_width);
            ir_builder.emit(SIRInstruction::Mux(result, cond_reg, then_val, else_val));
            self.stack.push_back(result);
            return Ok(());
        }

        let pre_ternary_defined = self.defined_ranges.clone();
        let pre_ternary_dynamic = self.dynamic_defined_vars.clone();

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

        let then_block = ir_builder.new_block();
        let carried_then = ir_builder.alloc_logic(result_width);
        let needs_merge = ir_builder.alloc_bit(1, false);
        let else_block = ir_builder.new_block_with(vec![carried_then, needs_merge]);
        let result = ir_builder.alloc_logic(result_width);
        let merge_block = ir_builder.new_block_with(vec![result]);

        ir_builder.seal_block(SIRTerminator::Branch {
            cond: known_false,
            true_block: (else_block, vec![dummy_then, direct_else]),
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
            branch_context,
        )?;
        let then_val = self.stack.pop_back().unwrap();
        let then_defined = std::mem::replace(&mut self.defined_ranges, pre_ternary_defined.clone());
        let then_dynamic =
            std::mem::replace(&mut self.dynamic_defined_vars, pre_ternary_dynamic.clone());
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: known_true,
            true_block: (merge_block, vec![then_val]),
            false_block: (else_block, vec![then_val, merge_else]),
        });

        ir_builder.switch_to_block(else_block);
        self.parse_expression_in_context(
            els,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            branch_context,
        )?;
        let else_val = self.stack.pop_back().unwrap();
        let else_defined = std::mem::take(&mut self.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.dynamic_defined_vars);
        let merged = ir_builder.alloc_logic(result_width);
        ir_builder.emit(SIRInstruction::Mux(
            merged,
            cond_reg,
            carried_then,
            else_val,
        ));
        let direct_else_block = ir_builder.new_block();
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: needs_merge,
            true_block: (merge_block, vec![merged]),
            false_block: (direct_else_block, vec![]),
        });

        ir_builder.switch_to_block(direct_else_block);
        ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![else_val]));

        ir_builder.switch_to_block(merge_block);
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
                use crate::parser::bitaccess::eval_constexpr;
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
    use crate::parser::BuildConfig;
    use veryl_analyzer::{
        Analyzer, Context, attribute_table,
        ir::{Component, Declaration, Ir},
        symbol_table,
    };
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;

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
                .analyze_pass2("prj", &parsed.veryl, &mut context, Some(&mut ir))
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
