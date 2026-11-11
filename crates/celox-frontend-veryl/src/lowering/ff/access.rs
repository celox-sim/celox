//! Variable access: type and register selects, guarded offsets, loads and stores.

use super::*;

/// Where the stores of this block are placed when they do not go to the region of the
/// domain: set once by the caller before lowering, read by every store.
pub(super) struct WritePlacement {
    pub(super) sparse_write_vars: HashSet<VarId>,
    pub(super) direct_static_write_ranges: HashMap<VarId, Vec<celox_design::BitAccess>>,
    pub(super) direct_dynamic_write_vars: HashSet<VarId>,
}

impl<'a> FfParser<'a> {
    fn eval_type_select(
        &self,
        typ: &Type,
        index: &VarIndex,
        select: &VarSelect,
    ) -> Option<BitAccess> {
        let width = typ.width();
        let mut dims = Vec::with_capacity(typ.array.as_slice().len() + width.as_slice().len() + 1);
        for dimension in typ.array.as_slice() {
            dims.push((*dimension)?);
        }
        if width.is_empty() {
            if let Some(kind_width) = typ.kind.width() {
                dims.push(kind_width);
            }
        } else {
            for dimension in width.iter() {
                dims.push((*dimension)?);
            }
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

        let mut base_offset = 0usize;
        let mut processed_count = 0usize;
        let index_count =
            (index.0.len() + select.0.len()).saturating_sub(usize::from(select.1.is_some()));

        for (i, index_val) in index
            .0
            .iter()
            .chain(&select.0)
            .take(index_count)
            .enumerate()
        {
            let idx = to_u(index_val)?;
            let stride = *strides.get(i)?;
            base_offset += idx * stride;
            processed_count += 1;
        }

        if let Some((op, range_expr)) = &select.1 {
            let anchor_expr = select.0.last()?;
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

    pub(super) fn emit_register_slice<A>(
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

    pub(super) fn eval_formal_type_select(
        &self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
    ) -> Option<BitAccess> {
        let formal_type = &self.module.variables.get(&var_id)?.r#type;
        self.eval_type_select(formal_type, index, select)
    }

    /// Compute the offset of a select, and record in `guard` when every
    /// runtime index addresses an existing element or bit.
    ///
    /// The offset alone does not bound the access: an out-of-range index
    /// would address a neighbouring element, or memory outside the variable.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_guarded_offset_calc<A>(
        &mut self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        mut guard: Option<&mut AccessGuard>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<SIROffset, ParserError> {
        // Keep unpacked-array indexing separate from packed bit selection.
        // Backends may give array elements a physical stride different from
        // their logical packed width, so combining both here loses essential
        // source-type information.
        let geometry = crate::bitaccess::select_geometry(self.module, var_id, index, select)?;
        let strides = &geometry.strides;
        let total_width = geometry.total_width;
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
                let index_reg = self.emit_index_term(
                    expr,
                    &mut dummy_targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                if let Some(guard) = guard.as_deref_mut() {
                    let outer = if i == 0 { total_width } else { strides[i - 1] };
                    let limit = outer / stride.max(1);
                    // A negative index is sign-extended when it is scaled, so
                    // its raw bits do not bound the address.
                    guard.clamp |= expr.comptime().r#type.signed
                        || index_bits_exceed(ir_builder.register(&index_reg).width(), limit);
                    and_index_guard(
                        &mut guard.indices,
                        index_reg,
                        expr.comptime().r#type.signed,
                        Some(limit),
                        ir_builder,
                    );
                }
                let term_reg = scale_offset(index_reg, scale, ir_builder);
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
                let part_elements = if i == select_len - 1 {
                    match geometry.part {
                        Some(
                            PartSelectGeometry::PlusColon { elements }
                            | PartSelectGeometry::MinusColon { elements }
                            | PartSelectGeometry::Step { elements },
                        ) => Some(elements),
                        _ => None,
                    }
                } else {
                    None
                };
                let index_reg = match (guard.as_deref_mut(), &select.1, part_elements) {
                    (Some(guard), Some((VarSelectOp::MinusColon, _)), Some(elements))
                        if i == select_len - 1 =>
                    {
                        // `anchor - (elements - 1)` is negative for a part
                        // select that starts below bit 0. Form it with 64-bit
                        // wrapping arithmetic so the low elements compare as
                        // out of range and the higher ones as in range.
                        let anchor = self.emit_index_term(
                            expr,
                            &mut dummy_targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                        and_index_guard(
                            &mut guard.indices,
                            anchor,
                            expr.comptime().r#type.signed,
                            None,
                            ir_builder,
                        );
                        let span = ir_builder.alloc_bit(64, false);
                        ir_builder.emit(SIRInstruction::Imm(
                            span,
                            SIRValue::new(elements as u64 - 1),
                        ));
                        let lsb = ir_builder.alloc_bit(64, false);
                        ir_builder.emit(SIRInstruction::Binary(lsb, anchor, BinaryOp::Sub, span));
                        lsb
                    }
                    _ => self.emit_index_term(
                        &selected_expr,
                        &mut dummy_targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?,
                };
                if let Some(guard) = guard.as_deref_mut() {
                    let outer = if dimension == 0 {
                        total_width
                    } else {
                        strides[dimension - 1]
                    };
                    let dimension_width = outer / stride.max(1);
                    // The `-:` lower bound above is unsigned; its anchor
                    // has already been checked.
                    let signed = selected_expr.comptime().r#type.signed
                        && !matches!(select.1, Some((VarSelectOp::MinusColon, _)));
                    match part_elements {
                        Some(elements) if elements > 1 => {
                            guard.part = Some(PartGuard {
                                lsb: index_reg,
                                signed,
                                elements,
                                dimension_width,
                                element_bits: scale,
                            });
                        }
                        _ => {
                            guard.clamp |= signed
                                || index_bits_exceed(
                                    ir_builder.register(&index_reg).width(),
                                    dimension_width,
                                );
                            and_index_guard(
                                &mut guard.indices,
                                index_reg,
                                signed,
                                Some(dimension_width),
                                ir_builder,
                            )
                        }
                    }
                }
                let term_reg = scale_offset(index_reg, scale, ir_builder);
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

    /// Helper: evaluates a runtime index expression.
    pub(super) fn emit_index_term<A>(
        &mut self,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
        Ok(self.stack.pop_back().unwrap())
    }

    /// Select `index`/`select` of `var_id` from a register holding the
    /// variable's whole value.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_register_select<A>(
        &mut self,
        value: RegisterId,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        if index.0.is_empty() && select.0.is_empty() && select.1.is_none() {
            return Ok(value);
        }
        let width = get_access_width(self.module, var_id, index, select)?;
        let mut guard = AccessGuard::default();
        let offset = self.emit_guarded_offset_calc(
            var_id,
            index,
            select,
            domain,
            convert,
            sources,
            Some(&mut guard),
            ir_builder,
        )?;
        let selected = match offset {
            SIROffset::Static(lsb)
            | SIROffset::PackedElements {
                bit_offset: lsb, ..
            } => self.emit_register_slice(value, BitAccess::new(lsb, lsb + width - 1), ir_builder),
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
        };
        // Slicing a register never leaves it, but an invalid index reads X
        // (or 0), not a neighbouring element (IEEE 1800-2023 7.4.6). Bits of
        // a part select past the register read 0.
        let condition = if guard.part.is_some() {
            guard.indices
        } else {
            guard.whole(ir_builder)
        };
        Ok(match condition {
            Some(condition) => {
                let unknown = unknown_like(ir_builder, selected, width);
                let guarded = alloc_like(ir_builder, selected, width);
                ir_builder.emit(SIRInstruction::Mux(guarded, condition, selected, unknown));
                guarded
            }
            None => selected,
        })
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
            let selected = self.emit_register_select(
                value, var_id, index, select, domain, convert, sources, ir_builder,
            )?;
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

        let mut guard = AccessGuard::default();
        let mut offset = self.emit_guarded_offset_calc(
            var_id,
            index,
            select,
            domain,
            convert,
            sources,
            Some(&mut guard),
            ir_builder,
        )?;
        if !source_type.array.is_empty()
            && index.0.is_empty()
            && select.0.is_empty()
            && select.1.is_none()
        {
            // A whole unpacked array is a multi-element transfer, like the
            // corresponding whole-array store in `op_store`.
            let element_count = source_type.total_array().ok_or_else(|| {
                ParserError::unresolved_width(
                    self.module,
                    &self.module.variables[&var_id],
                    source_type.to_string(),
                )
            })?;
            if element_count > 0 && width.is_multiple_of(element_count) {
                offset = SIROffset::PackedElements {
                    bit_offset: 0,
                    element_width: width / element_count,
                };
            }
        }
        let is_internal = self.local_working_vars.contains(&var_id)
            || self.inline_function_locals.contains(&var_id);
        let load_region = if is_internal {
            WORKING_REGION
        } else {
            STABLE_REGION
        };
        let dest_reg = emit_guarded_load(
            ir_builder,
            &guard,
            dest_reg,
            || convert(var_id, load_region),
            offset,
            width,
        );

        self.stack.push_back(dest_reg);

        // For source tracking, use the conservative range from eval_var_select
        // (covers all bits that might be read by a dynamic index).
        let access = eval_var_select(self.module, var_id, index, select)?;
        // Module state read in an always_ff block is always the pre-edge
        // value, even when an earlier statement in the same block assigned
        // that variable.  `defined_ranges` only describes pending writes; it
        // must not hide the old-state read from the shared-clock scheduler.
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

        let mut guard = AccessGuard::default();
        let mut offset = self.emit_guarded_offset_calc(
            dst.id,
            &dst.index,
            &dst.select,
            domain,
            convert,
            sources,
            Some(&mut guard),
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
        if self.inline_function_locals.contains(&dst.id) {
            // Inline function storage is blocking and call-private, so it
            // bypasses the FF target, seed, and commit bookkeeping.
            emit_guarded_store(
                ir_builder,
                &guard,
                || convert(dst.id, WORKING_REGION),
                offset,
                target_width,
                src_reg,
            );
            return Ok(());
        }
        let store_region = if matches!(domain, Domain::Ff)
            && (!is_static || self.placement.sparse_write_vars.contains(&dst.id))
        {
            SPARSE_WORKING_REGION
        } else {
            domain.region()
        };
        let direct_write = if is_static {
            self.placement
                .direct_static_write_ranges
                .get(&dst.id)
                .is_some_and(|ranges| {
                    ranges
                        .iter()
                        .any(|range| range.lsb <= access.lsb && range.msb >= access.msb)
                })
        } else {
            self.placement.direct_dynamic_write_vars.contains(&dst.id)
        };
        let store_region =
            if direct_write && matches!(store_region, WORKING_REGION | SPARSE_WORKING_REGION) {
                STABLE_REGION
            } else {
                store_region
            };
        emit_guarded_store(
            ir_builder,
            &guard,
            || convert(dst.id, store_region),
            offset,
            target_width,
            src_reg,
        );

        // Use conservative range from eval_var_select for tracking (covers all possible bits).
        if is_static {
            let bits = self.defined.defined_ranges.entry(dst.id).or_default();
            for i in access.lsb..=access.msb {
                bits.insert(i);
            }
        } else {
            self.dynamic_write_vars.insert(dst.id);
        }
        self.defined.dynamic_defined_vars.insert(dst.id);

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
}
