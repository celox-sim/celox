//! Input loads, dynamic offsets, and scoped input overrides.

use super::*;

impl SLTToSIRLowerer {
    pub(super) fn lower_compacted_input<
        A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display,
    >(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        variable: &A,
        index: &[crate::SLTIndex],
        width: usize,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
    ) -> RegisterId {
        if index.is_empty()
            && let Some(&element_width) = self.unpacked_input_element_widths.get(&node)
            && width.is_multiple_of(element_width)
        {
            let destination = builder.alloc_logic(width);
            builder.emit(SIRInstruction::Load(
                destination,
                variable.clone(),
                SIROffset::PackedElements {
                    bit_offset: 0,
                    element_width,
                },
                width,
            ));
            destination
        } else {
            self.lower_input_for_node(
                builder,
                node,
                variable,
                index,
                &BitAccess::new(0, width - 1),
                arena,
                cache,
                None,
            )
        }
    }

    pub(super) fn lower_input_for_node<
        A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display,
    >(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        id: &A,
        index: &[crate::SLTIndex],
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
    ) -> RegisterId {
        let width = access.msb - access.lsb + 1;
        if index.is_empty()
            && let Some(&element_width) = self.unpacked_input_element_widths.get(&node)
            && element_width != 0
            && width > element_width
            && access.lsb.is_multiple_of(element_width)
            && width.is_multiple_of(element_width)
        {
            let destination = builder.alloc_logic(width);
            builder.emit(SIRInstruction::Load(
                destination,
                id.clone(),
                SIROffset::PackedElements {
                    bit_offset: access.lsb,
                    element_width,
                },
                width,
            ));
            destination
        } else {
            self.lower_input(builder, id, index, access, arena, cache, env)
        }
    }

    pub(super) fn lower_input<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        id: &A,
        index: &[crate::SLTIndex],
        access: &BitAccess,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
    ) -> RegisterId {
        let width = access.msb - access.lsb + 1;
        let dest = builder.alloc_logic(width);

        if !index.is_empty() {
            // Analyzer-unrolled array accesses retain their index syntax even
            // when every index expression is a compile-time constant.  Fold
            // those accesses back to one logical static bit offset here.  In
            // particular, this lets the native element-strided layout choose
            // a direct address instead of materializing a fake dynamic index
            // for every unrolled lane.
            let static_offset = index.iter().try_fold(access.lsb, |offset, entry| {
                let (value, mask) = try_const_eval(entry.node, arena)?;
                if !mask.is_zero() {
                    return None;
                }
                let value = value.to_usize()?;
                offset.checked_add(value.checked_mul(entry.stride)?)
            });
            if let Some(static_offset) = static_offset {
                builder.emit(SIRInstruction::Load(
                    dest,
                    id.clone(),
                    SIROffset::Static(static_offset),
                    width,
                ));
                return dest;
            }

            let element_width = index.iter().find_map(|entry| match entry.kind {
                crate::SLTIndexKind::Unpacked { element_width } => Some(element_width),
                crate::SLTIndexKind::Packed => None,
            });
            let element_access = element_width.filter(|element_width| {
                access.msb < *element_width
                    && index.iter().all(|entry| match entry.kind {
                        crate::SLTIndexKind::Unpacked {
                            element_width: width,
                        } => width == *element_width && entry.stride % *element_width == 0,
                        crate::SLTIndexKind::Packed => entry.stride < *element_width,
                    })
            });
            let mut element_dynamic = None;
            let mut packed_dynamic = None;
            let mut logical_dynamic = None;
            for idx_entry in index {
                let mut idx_val =
                    self.lower_inner(builder, idx_entry.node, arena, cache, env, env.is_none());

                let (stride, accumulator) = if let Some(element_width) = element_access {
                    match idx_entry.kind {
                        crate::SLTIndexKind::Unpacked { .. } => {
                            (idx_entry.stride / element_width, &mut element_dynamic)
                        }
                        crate::SLTIndexKind::Packed => (idx_entry.stride, &mut packed_dynamic),
                    }
                } else {
                    (idx_entry.stride, &mut logical_dynamic)
                };
                if stride > 1 {
                    let stride_reg = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Imm(
                        stride_reg,
                        SIRValue::new(stride as u64),
                    ));
                    let stepped_idx = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Binary(
                        stepped_idx,
                        idx_val,
                        BinaryOp::Mul,
                        stride_reg,
                    ));
                    idx_val = stepped_idx;
                }

                if let Some(acc) = *accumulator {
                    let new_acc = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Binary(new_acc, acc, BinaryOp::Add, idx_val));
                    *accumulator = Some(new_acc);
                } else {
                    *accumulator = Some(idx_val);
                }
            }

            let offset = if let Some(element_width) = element_access {
                if let Some(element_index) = element_dynamic {
                    SIROffset::Element {
                        index: element_index,
                        element_width,
                        bit_offset: access.lsb,
                        dynamic_bit_offset: packed_dynamic,
                    }
                } else {
                    unreachable!("an unpacked element access has an unpacked index")
                }
            } else if let Some(dynamic_off) = logical_dynamic {
                if access.lsb == 0 {
                    SIROffset::Dynamic(dynamic_off)
                } else {
                    let static_off = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Imm(
                        static_off,
                        SIRValue::new(access.lsb as u64),
                    ));
                    let final_off = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Binary(
                        final_off,
                        static_off,
                        BinaryOp::Add,
                        dynamic_off,
                    ));
                    SIROffset::Dynamic(final_off)
                }
            } else {
                SIROffset::Static(access.lsb)
            };
            builder.emit(SIRInstruction::Load(dest, id.clone(), offset, width));
        } else {
            builder.emit(SIRInstruction::Load(
                dest,
                id.clone(),
                SIROffset::Static(access.lsb),
                width,
            ));
        }

        dest
    }

    fn build_dynamic_offset<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: Option<&LowerEnv<'_, A>>,
        index: &[crate::SLTIndex],
        access: &BitAccess,
    ) -> RegisterId {
        let off_reg = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Imm(
            off_reg,
            SIRValue::new(access.lsb as u64),
        ));

        let mut total_dynamic = None;
        for idx_entry in index {
            let mut idx_val =
                self.lower_inner(builder, idx_entry.node, arena, cache, env, env.is_none());

            if idx_entry.stride > 1 {
                let stride_reg = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Imm(
                    stride_reg,
                    SIRValue::new(idx_entry.stride as u64),
                ));
                let stepped_idx = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(
                    stepped_idx,
                    idx_val,
                    BinaryOp::Mul,
                    stride_reg,
                ));
                idx_val = stepped_idx;
            }

            if let Some(acc) = total_dynamic {
                let new_acc = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(new_acc, acc, BinaryOp::Add, idx_val));
                total_dynamic = Some(new_acc);
            } else {
                total_dynamic = Some(idx_val);
            }
        }

        if let Some(dynamic_off) = total_dynamic {
            let final_off = builder.alloc_bit(64, false);
            builder.emit(SIRInstruction::Binary(
                final_off,
                off_reg,
                BinaryOp::Add,
                dynamic_off,
            ));
            final_off
        } else {
            off_reg
        }
    }

    fn rebuild_override_range<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: &LowerEnv<'_, A>,
        id: &A,
        index: &[crate::SLTIndex],
        access: &BitAccess,
    ) -> Option<RegisterId> {
        let exact = VarAtomBase::new(id.clone(), access.lsb, access.msb);
        let mut higher_priority_overlap = false;
        let mut layer = Some(env);
        while let Some(current) = layer {
            if !higher_priority_overlap && let Some(reg) = current.inputs.get(&exact) {
                return Some(*reg);
            }
            for (target, reg) in &current.inputs {
                if target.id != *id {
                    continue;
                }
                if !higher_priority_overlap
                    && target.access.lsb <= access.lsb
                    && access.msb <= target.access.msb
                {
                    let rel = BitAccess::new(
                        access.lsb - target.access.lsb,
                        access.msb - target.access.lsb,
                    );
                    return Some(self.slice_reg(builder, *reg, &rel));
                }
            }
            higher_priority_overlap |= current.inputs.keys().any(|target| {
                target.id == *id
                    && target.access.lsb <= access.msb
                    && access.lsb <= target.access.msb
            });
            layer = current.parent;
        }

        let mut cut_points = vec![access.lsb, access.msb + 1];
        let mut layer = Some(env);
        while let Some(current) = layer {
            for target in current.inputs.keys() {
                if target.id != *id {
                    continue;
                }
                if target.access.msb < access.lsb || access.msb < target.access.lsb {
                    continue;
                }
                cut_points.push(target.access.lsb.max(access.lsb));
                cut_points.push((target.access.msb + 1).min(access.msb + 1));
            }
            layer = current.parent;
        }
        cut_points.sort_unstable();
        cut_points.dedup();
        if cut_points.len() <= 2 {
            return None;
        }

        let mut part_regs = Vec::new();
        for window in cut_points.windows(2).rev() {
            let part_access = BitAccess::new(window[0], window[1] - 1);
            let mut part_reg = None;
            let mut layer = Some(env);
            'layers: while let Some(current) = layer {
                for (target, reg) in &current.inputs {
                    if target.id != *id {
                        continue;
                    }
                    if target.access.lsb <= part_access.lsb && part_access.msb <= target.access.msb
                    {
                        let rel = BitAccess::new(
                            part_access.lsb - target.access.lsb,
                            part_access.msb - target.access.lsb,
                        );
                        part_reg = Some(self.slice_reg(builder, *reg, &rel));
                        break 'layers;
                    }
                }
                layer = current.parent;
            }
            let reg = part_reg.unwrap_or_else(|| {
                self.lower_input_for_node(
                    builder,
                    node,
                    id,
                    index,
                    &part_access,
                    arena,
                    cache,
                    None,
                )
            });
            part_regs.push(reg);
        }

        if part_regs.len() == 1 {
            part_regs.into_iter().next()
        } else {
            let result = builder.alloc_logic(access.msb - access.lsb + 1);
            builder.emit(SIRInstruction::Concat(result, part_regs));
            Some(result)
        }
    }

    pub(super) fn lookup_override<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        &self,
        builder: &mut SIRBuilder<A>,
        node: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        env: &LowerEnv<'_, A>,
        id: &A,
        index: &[crate::SLTIndex],
        access: &BitAccess,
    ) -> Option<RegisterId> {
        if !index.is_empty() {
            let mut layer = Some(env);
            let mut has_override = false;
            while let Some(current) = layer {
                has_override |= current.inputs.keys().any(|target| target.id == *id);
                layer = current.parent;
            }
            if !has_override {
                return Some(self.lower_input(builder, id, index, access, arena, cache, Some(env)));
            }

            let dynamic_off =
                self.build_dynamic_offset(builder, arena, cache, Some(env), index, access);
            let mut result = self.lower_input(builder, id, index, access, arena, cache, Some(env));
            let result_width = access.msb - access.lsb + 1;
            let mut layers = Vec::new();
            let mut layer = Some(env);
            while let Some(current) = layer {
                layers.push(current);
                layer = current.parent;
            }
            // Apply outer bindings first and inner bindings last so an inner
            // loop-carried range wins whenever scopes overlap.
            for current in layers.into_iter().rev() {
                for (target, reg) in &current.inputs {
                    if target.id != *id {
                        continue;
                    }
                    let range_lo = target.access.lsb;
                    let Some(range_hi) = target
                        .access
                        .msb
                        .checked_sub(result_width.saturating_sub(1))
                    else {
                        continue;
                    };
                    if range_lo > range_hi {
                        continue;
                    }

                    let lo_reg = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Imm(lo_reg, SIRValue::new(range_lo as u64)));
                    let hi_reg = builder.alloc_bit(64, false);
                    builder.emit(SIRInstruction::Imm(hi_reg, SIRValue::new(range_hi as u64)));

                    let ge_lo = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        ge_lo,
                        dynamic_off,
                        BinaryOp::GeU,
                        lo_reg,
                    ));
                    let le_hi = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        le_hi,
                        dynamic_off,
                        BinaryOp::LeU,
                        hi_reg,
                    ));
                    let in_range = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        in_range,
                        ge_lo,
                        BinaryOp::And,
                        le_hi,
                    ));

                    let rel_off = if range_lo == 0 {
                        dynamic_off
                    } else {
                        let rel = builder.alloc_bit(64, false);
                        builder.emit(SIRInstruction::Binary(
                            rel,
                            dynamic_off,
                            BinaryOp::Sub,
                            lo_reg,
                        ));
                        rel
                    };

                    let shifted = builder.alloc_logic(target.access.msb - target.access.lsb + 1);
                    builder.emit(SIRInstruction::Binary(
                        shifted,
                        *reg,
                        BinaryOp::Shr,
                        rel_off,
                    ));
                    let candidate = self.cast_reg_width(builder, shifted, result_width);
                    let merged = builder.alloc_logic(result_width);
                    builder.emit(SIRInstruction::Mux(merged, in_range, candidate, result));
                    result = merged;
                }
            }
            return Some(result);
        }
        self.rebuild_override_range(builder, node, arena, cache, env, id, index, access)
    }
}
