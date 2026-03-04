use cranelift::codegen::ir::{BlockArg, FuncRef, StackSlotData, StackSlotKind};
use cranelift::prelude::*;
use cranelift_frontend::FunctionBuilder;

use crate::{
    HashMap, SimulatorOptions,
    ir::{
        AbsoluteAddr, BinaryOp, BlockId, RegionedAbsoluteAddr, RegisterId, RegisterType,
        SIRInstruction, STABLE_REGION,
    },
    optimizer::coalescing::TailCallChunk,
    optimizer::coalescing::pass_tail_call_split::{SpillSlot, SpilledChunk},
};

use super::MemoryLayout;

/// Pre-load trigger signal values at function entry for register-based
/// edge detection. Only needed when `emit_triggers` is enabled (Simulation mode).
///
/// Scans the given blocks for Store/Commit instructions with triggers,
/// loads their current values from memory, and returns the old-value map.
fn preload_trigger_old_values<'a>(
    blocks: impl Iterator<Item = &'a crate::ir::BasicBlock<RegionedAbsoluteAddr>>,
    builder: &mut FunctionBuilder,
    mem_ptr: Value,
    layout: &MemoryLayout,
    emit_triggers: bool,
) -> HashMap<(AbsoluteAddr, u32), Value> {
    if !emit_triggers {
        return HashMap::default();
    }

    let mut trigger_addrs: std::collections::HashSet<(AbsoluteAddr, u32)> =
        std::collections::HashSet::new();
    for block in blocks {
        for inst in &block.instructions {
            match inst {
                SIRInstruction::Store(addr, _, _, _, triggers) if !triggers.is_empty() => {
                    trigger_addrs.insert((addr.absolute_addr(), addr.region));
                }
                SIRInstruction::Commit(_, dst, _, _, triggers) if !triggers.is_empty() => {
                    trigger_addrs.insert((dst.absolute_addr(), dst.region));
                }
                _ => {}
            }
        }
    }

    let mut old_values: HashMap<(AbsoluteAddr, u32), Value> = HashMap::default();
    for (abs, region) in trigger_addrs {
        let width = layout.widths[&abs];
        debug_assert!(
            width <= 64,
            "Trigger signal wider than 64 bits is not supported"
        );
        let cl_type = get_cl_type(width);
        let base_offset = if region == STABLE_REGION {
            layout.offsets[&abs]
        } else {
            layout.working_base_offset + layout.working_offsets[&abs]
        };
        let addr_val = builder.ins().iadd_imm(mem_ptr, base_offset as i64);
        let raw_val = builder.ins().load(cl_type, MemFlags::new(), addr_val, 0);
        let val = if cl_type == types::I64 {
            raw_val
        } else {
            builder.ins().uextend(types::I64, raw_val)
        };
        old_values.insert((abs, region), val);
    }
    old_values
}

/// Minimum number of 64-bit chunks to use memory-backed shift/sar.
/// Below this, the O(n²) select-chain is cheaper at runtime.
pub const MEM_SHIFT_THRESHOLD: usize = 4; // 256-bit+

#[derive(Clone)]
pub enum TransValue {
    TwoState(Vec<Value>),
    FourState {
        values: Vec<Value>,
        masks: Vec<Value>,
    },
    /// Memory-backed wide value stored in a Cranelift stack slot.
    /// Used for wide shift/sar results to avoid O(n²) instruction scaling.
    MemBacked {
        addr: Value,
        num_chunks: usize,
        mask_addr: Option<Value>,
    },
}

impl TransValue {
    /// Load all value chunks as registers. Works for all variants.
    pub fn load_value_chunks(&self, builder: &mut FunctionBuilder) -> Vec<Value> {
        match self {
            TransValue::TwoState(v) => v.clone(),
            TransValue::FourState { values, .. } => values.clone(),
            TransValue::MemBacked {
                addr, num_chunks, ..
            } => (0..*num_chunks)
                .map(|i| {
                    builder
                        .ins()
                        .load(types::I64, MemFlags::new(), *addr, (i * 8) as i32)
                })
                .collect(),
        }
    }

    /// Load mask chunks as registers. Works for all variants.
    pub fn load_mask_chunks(&self, builder: &mut FunctionBuilder) -> Option<Vec<Value>> {
        match self {
            TransValue::TwoState(_) => None,
            TransValue::FourState { masks, .. } => Some(masks.clone()),
            TransValue::MemBacked {
                mask_addr,
                num_chunks,
                ..
            } => mask_addr.map(|ma| {
                (0..*num_chunks)
                    .map(|i| {
                        builder
                            .ins()
                            .load(types::I64, MemFlags::new(), ma, (i * 8) as i32)
                    })
                    .collect()
            }),
        }
    }

    /// Get the first value chunk. For MemBacked, loads from memory.
    /// Use this instead of `values()[0]` when the value might be MemBacked.
    pub fn first_value(&self, builder: &mut FunctionBuilder) -> Value {
        match self {
            TransValue::TwoState(v) => v[0],
            TransValue::FourState { values, .. } => values[0],
            TransValue::MemBacked { addr, .. } => {
                builder.ins().load(types::I64, MemFlags::new(), *addr, 0)
            }
        }
    }

    /// Get the first mask chunk. For MemBacked, loads from memory.
    /// Use this instead of `masks().map(|m| m[0])` when the value might be MemBacked.
    pub fn first_mask(&self, builder: &mut FunctionBuilder) -> Option<Value> {
        match self {
            TransValue::TwoState(_) => None,
            TransValue::FourState { masks, .. } => Some(masks[0]),
            TransValue::MemBacked { mask_addr, .. } => {
                mask_addr.map(|ma| builder.ins().load(types::I64, MemFlags::new(), ma, 0))
            }
        }
    }
}

/// Create a new stack slot for `num_chunks` i64 values and return the base address.
pub fn alloc_stack_slot(builder: &mut FunctionBuilder, num_chunks: usize) -> (StackSlot, Value) {
    let slot = builder.create_sized_stack_slot(StackSlotData::new(
        StackSlotKind::ExplicitSlot,
        (num_chunks * 8) as u32,
        3,
    ));
    let addr = builder.ins().stack_addr(types::I64, slot, 0);
    (slot, addr)
}

use cranelift::codegen::ir::StackSlot;

pub struct SIRTranslator {
    pub layout: MemoryLayout,
    pub options: SimulatorOptions,
}

/// Temporary state used only during translation
pub struct TranslationState<'a, 'b, 'c> {
    pub builder: &'a mut FunctionBuilder<'b>,
    pub regs: HashMap<RegisterId, TransValue>,
    pub mem_ptr: Value,
    pub register_map: &'c HashMap<RegisterId, RegisterType>,
    /// Pre-loaded trigger signal values (old values captured at function entry).
    /// Key: (AbsoluteAddr, region). Value: i64 SSA value of the full signal.
    pub trigger_old_values: HashMap<(AbsoluteAddr, u32), Value>,
    /// Tracks SIR Imm constants (2-state, ≤64-bit) for peephole optimizations
    /// such as shift-by-0 elimination.
    pub imm_constants: HashMap<RegisterId, u64>,
}

pub(crate) fn get_cl_type(width: usize) -> Type {
    if width <= 8 {
        types::I8
    } else if width <= 16 {
        types::I16
    } else if width <= 32 {
        types::I32
    } else {
        types::I64
    }
}

pub(crate) fn promote_to_physical(
    state: &mut TranslationState,
    val: Value,
    src_logical_width: usize,
    is_signed: bool,
    dst_phys_ty: Type,
) -> Value {
    let src_phys_ty = state.builder.func.dfg.value_type(val);

    let val = if src_phys_ty == dst_phys_ty {
        val
    } else if src_phys_ty.bits() > dst_phys_ty.bits() {
        state.builder.ins().ireduce(dst_phys_ty, val)
    } else if is_signed {
        state.builder.ins().sextend(dst_phys_ty, val)
    } else {
        state.builder.ins().uextend(dst_phys_ty, val)
    };

    let phys_bits = dst_phys_ty.bits() as i64;

    if src_logical_width < phys_bits as usize {
        if is_signed {
            let shift_amt = phys_bits - (src_logical_width as i64);
            let tmp = state.builder.ins().ishl_imm(val, shift_amt);
            state.builder.ins().sshr_imm(tmp, shift_amt)
        } else {
            let mask_val = (1u64 << src_logical_width).wrapping_sub(1);
            let mask = state.builder.ins().iconst(dst_phys_ty, mask_val as i64);
            state.builder.ins().band(val, mask)
        }
    } else {
        val
    }
}

pub(crate) fn cast_type(builder: &mut FunctionBuilder, val: Value, target_ty: Type) -> Value {
    let current_ty = builder.func.dfg.value_type(val);

    if current_ty.bits() > target_ty.bits() {
        // e.g., i64 -> i32 (discard upper bits)
        builder.ins().ireduce(target_ty, val)
    } else if current_ty.bits() < target_ty.bits() {
        // e.g., i8 -> i32 (zero-fill upper bits)
        builder.ins().uextend(target_ty, val)
    } else {
        // Use as-is if types are the same
        val
    }
}

pub(crate) fn get_chunk_as_i64(builder: &mut FunctionBuilder, chunks: &[Value], i: usize) -> Value {
    if chunks.is_empty() {
        return builder.ins().iconst(types::I64, 0);
    }

    // If multi-word expansion is already applied
    if chunks.len() > 1 {
        return chunks
            .get(i)
            .copied()
            .unwrap_or_else(|| builder.ins().iconst(types::I64, 0));
    }

    // For single Value (i8 ~ i128)
    let val = chunks[0];
    let val_ty = builder.func.dfg.value_type(val);
    if i == 0 {
        // i8~i64 to i64 (assumed to be uextend/ireduce in cast_type)
        cast_type(builder, val, types::I64)
    } else if val_ty == types::I128 && i == 1 {
        let upper = builder.ins().ushr_imm(val, 64);
        builder.ins().ireduce(types::I64, upper)
    } else {
        builder.ins().iconst(types::I64, 0)
    }
}

use super::control::collect_block_param_types;

impl SIRTranslator {
    fn translate_instruction(
        &self,
        state: &mut TranslationState,
        inst: &SIRInstruction<RegionedAbsoluteAddr>,
    ) {
        match inst {
            SIRInstruction::Imm(dst, val) => {
                self.translate_imm_inst(state, dst, val);
                // Track 2-state constants that fit in u64 for peephole opts
                if val.mask.to_u64_digits().is_empty() {
                    let digits = val.payload.to_u64_digits();
                    if digits.len() <= 1 {
                        state
                            .imm_constants
                            .insert(*dst, digits.first().copied().unwrap_or(0));
                    }
                }
            }
            SIRInstruction::Concat(dst, args) => {
                self.translate_concat_inst(state, dst, args);
            }
            SIRInstruction::Binary(dst, lhs, op, rhs) => {
                if matches!(op, BinaryOp::Shr | BinaryOp::Shl | BinaryOp::Sar)
                    && state.imm_constants.get(rhs) == Some(&0)
                {
                    // Shift by 0 is identity — copy lhs to dst with width adjustment
                    let d_width = state.register_map[dst].width();
                    let l_val = state.regs[lhs].clone();
                    if d_width <= 64 {
                        let ty = get_cl_type(d_width);
                        let v = l_val.first_value(state.builder);
                        let cast = cast_type(state.builder, v, ty);
                        let masked = super::arith::apply_d_width_mask(state, cast, ty, d_width);
                        if self.options.four_state {
                            let m = l_val
                                .first_mask(state.builder)
                                .unwrap_or_else(|| state.builder.ins().iconst(ty, 0));
                            let m_cast = cast_type(state.builder, m, ty);
                            let m_masked =
                                super::arith::apply_d_width_mask(state, m_cast, ty, d_width);
                            state.regs.insert(
                                *dst,
                                TransValue::FourState {
                                    values: vec![masked],
                                    masks: vec![m_masked],
                                },
                            );
                        } else {
                            state.regs.insert(*dst, TransValue::TwoState(vec![masked]));
                        }
                    } else {
                        state.regs.insert(*dst, l_val);
                    }
                } else {
                    self.translate_binary_inst(state, dst, lhs, op, rhs);
                }
            }
            SIRInstruction::Unary(dst, op, rhs) => {
                self.translate_unary_inst(state, dst, op, rhs);
            }
            SIRInstruction::Load(dst, addr, offset, op_width) => {
                self.translate_load_inst(state, dst, addr, offset, op_width);
            }
            SIRInstruction::Store(addr, offset, op_width, src_reg, triggers) => {
                self.translate_store_inst(state, addr, offset, op_width, src_reg, triggers);
            }
            SIRInstruction::Commit(src_addr, dst_addr, offset, op_width, triggers) => {
                self.translate_commit_inst(state, src_addr, dst_addr, offset, op_width, triggers);
            }
        }
    }

    pub fn translate_units(
        &self,
        units: &[crate::ir::ExecutionUnit<RegionedAbsoluteAddr>],
        mut builder: FunctionBuilder,
    ) {
        // 1. Create function entry (entry block)
        // Here we create a "true entry" to connect all units
        let master_entry = builder.create_block();
        builder.append_block_params_for_function_params(master_entry);
        builder.switch_to_block(master_entry);
        if units.is_empty() {
            let r = builder.ins().iconst(types::I64, 0);
            builder.ins().return_(&[r]);
            builder.seal_all_blocks();
            builder.finalize();
            return;
        }

        // Get argument pointer
        let mem_ptr = builder.block_params(master_entry)[0];
        let mut unit_entry_blocks = Vec::new();
        for _ in units {
            unit_entry_blocks.push(builder.create_block());
        }
        if units.is_empty() {
            let r = builder.ins().iconst(types::I64, 0);
            builder.ins().return_(&[r]);
            builder.seal_all_blocks();
            builder.finalize();
            return;
        }

        let trigger_old_values = preload_trigger_old_values(
            units.iter().flat_map(|u| u.blocks.values()),
            &mut builder,
            mem_ptr,
            &self.layout,
            self.options.emit_triggers,
        );

        builder.ins().jump(unit_entry_blocks[0], &[]);
        // 2. Translate each ExecutionUnit in order
        for (i, unit) in units.iter().enumerate() {
            // --- Create "isolated" state for each unit ---
            let unit_entry = unit_entry_blocks[i];
            let next_unit_entry = if i + 1 < units.len() {
                Some(unit_entry_blocks[i + 1])
            } else {
                None
            };

            // Important: RegisterId is unique within a Unit, so clear regs for each Unit
            let mut state = TranslationState {
                builder: &mut builder,
                regs: HashMap::default(),
                mem_ptr,
                register_map: &unit.register_map,
                trigger_old_values: trigger_old_values.clone(),
                imm_constants: HashMap::default(),
            };

            // Create block map for this unit
            let mut block_map = HashMap::default();
            for (id, block) in &unit.blocks {
                let cl_bb = if id == &unit.entry_block_id {
                    unit_entry
                } else {
                    state.builder.create_block()
                };
                for &param_reg in &block.params {
                    let width = unit.register_map[&param_reg].width();
                    let ty = get_cl_type(width);
                    // Value block param
                    state.builder.append_block_param(cl_bb, ty);
                    // In 4-state mode, also append a mask block param
                    if self.options.four_state {
                        state.builder.append_block_param(cl_bb, ty);
                    }
                }
                block_map.insert(*id, cl_bb);
            }

            // Jump from the previous unit (or master entry) to the starting point of this unit
            let mut block_ids: Vec<_> = unit.blocks.keys().collect();
            block_ids.sort();
            // 3. Translate each block within the unit
            for id in &block_ids {
                let cl_block = block_map[id];
                state.builder.switch_to_block(cl_block);
                let cl_params = state.builder.block_params(cl_block);
                let sir_block = &unit.blocks[id];

                for (i, &sir_param_reg) in sir_block.params.iter().enumerate() {
                    let tval = if self.options.four_state {
                        // In 4-state mode, each SIR param maps to 2 Cranelift params: value + mask
                        let val = cl_params[i * 2];
                        let mask = cl_params[i * 2 + 1];
                        TransValue::FourState {
                            values: vec![val],
                            masks: vec![mask],
                        }
                    } else {
                        let val = cl_params[i];
                        TransValue::TwoState(vec![val])
                    };
                    state.regs.insert(sir_param_reg, tval);
                }
                for inst in &sir_block.instructions {
                    self.translate_instruction(&mut state, inst);
                }

                // Translate terminator
                // However, SIRTerminator::Return for units other than the last one
                // must be handled as "transition to the next unit" (described later)
                self.translate_terminator(
                    &mut state,
                    &sir_block.terminator,
                    &block_map,
                    next_unit_entry, // 最後のユニットかどうかを渡す
                );
            }
        }

        // Finally, seal all blocks
        builder.seal_all_blocks();
        builder.finalize();
    }

    /// Translate a single chunk of a tail-call chain.
    ///
    /// - `chunk`: the chunk being compiled
    /// - `next_chunk_func_ref`: if this is not the last chunk, the FuncRef to tail-call into
    /// - `next_chunk`: if this is not the last chunk, provides the next chunk's incoming live regs
    ///   so we know which registers to pass as arguments
    pub fn translate_chunk(
        &self,
        chunk: &TailCallChunk,
        next_chunk_func_ref: Option<FuncRef>,
        mut builder: FunctionBuilder,
    ) {
        let master_entry = builder.create_block();
        builder.append_block_params_for_function_params(master_entry);
        builder.switch_to_block(master_entry);

        let units = &chunk.units;
        if units.is_empty() {
            let r = builder.ins().iconst(types::I64, 0);
            builder.ins().return_(&[r]);
            builder.seal_all_blocks();
            builder.finalize();
            return;
        }

        // Multi-EU chunks must not carry incoming live regs — RegisterIds are
        // EU-scoped, so cross-EU register forwarding is not meaningful.
        debug_assert!(
            units.len() <= 1 || chunk.incoming_live_regs.is_empty(),
            "Multi-EU chunk with incoming live regs is not supported"
        );

        // Extract mem_ptr from param 0
        let params = builder.block_params(master_entry);
        let mem_ptr = params[0];

        // Extract incoming live regs from params 1..N
        let mut incoming_reg_values: HashMap<RegisterId, TransValue> = HashMap::default();
        let mut param_idx = 1;
        for (reg_id, reg_ty) in &chunk.incoming_live_regs {
            let width = reg_ty.width();
            let nc = width.div_ceil(64).max(1);
            if self.options.four_state {
                let mut values = Vec::with_capacity(nc);
                let mut masks = Vec::with_capacity(nc);
                for _ in 0..nc {
                    values.push(params[param_idx]);
                    param_idx += 1;
                    masks.push(params[param_idx]);
                    param_idx += 1;
                }
                incoming_reg_values.insert(*reg_id, TransValue::FourState { values, masks });
            } else {
                let mut values = Vec::with_capacity(nc);
                for _ in 0..nc {
                    values.push(params[param_idx]);
                    param_idx += 1;
                }
                incoming_reg_values.insert(*reg_id, TransValue::TwoState(values));
            }
        }

        let trigger_old_values = preload_trigger_old_values(
            units.iter().flat_map(|u| u.blocks.values()),
            &mut builder,
            mem_ptr,
            &self.layout,
            self.options.emit_triggers,
        );

        // Create unit entry blocks
        let mut unit_entry_blocks = Vec::new();
        for _ in units {
            unit_entry_blocks.push(builder.create_block());
        }

        builder.ins().jump(unit_entry_blocks[0], &[]);

        // Translate each EU, same as translate_units but with tail-call awareness
        for (i, unit) in units.iter().enumerate() {
            let unit_entry = unit_entry_blocks[i];
            let next_unit_entry = if i + 1 < units.len() {
                Some(unit_entry_blocks[i + 1])
            } else {
                None
            };

            let is_last_unit = i + 1 == units.len();

            // Build the tail-call info for the last EU's Return terminator
            let tail_call_info = if is_last_unit {
                next_chunk_func_ref.map(|func_ref| TailCallInfo {
                    func_ref,
                    mem_ptr,
                    outgoing_live_regs: chunk.outgoing_live_regs.clone(),
                })
            } else {
                None
            };

            let mut state = TranslationState {
                builder: &mut builder,
                regs: HashMap::default(),
                mem_ptr,
                register_map: &unit.register_map,
                trigger_old_values: trigger_old_values.clone(),
                imm_constants: HashMap::default(),
            };

            // Seed incoming live regs into the first EU's register state
            if i == 0 {
                for (reg_id, trans_val) in &incoming_reg_values {
                    state.regs.insert(*reg_id, trans_val.clone());
                }
            }

            let mut block_map = HashMap::default();
            for (id, block) in &unit.blocks {
                let cl_bb = if id == &unit.entry_block_id {
                    unit_entry
                } else {
                    state.builder.create_block()
                };
                for &param_reg in &block.params {
                    let width = unit.register_map[&param_reg].width();
                    let ty = get_cl_type(width);
                    state.builder.append_block_param(cl_bb, ty);
                    if self.options.four_state {
                        state.builder.append_block_param(cl_bb, ty);
                    }
                }
                block_map.insert(*id, cl_bb);
            }

            let mut block_ids: Vec<_> = unit.blocks.keys().collect();
            block_ids.sort();

            for id in &block_ids {
                let cl_block = block_map[id];
                state.builder.switch_to_block(cl_block);
                let cl_params = state.builder.block_params(cl_block);
                let sir_block = &unit.blocks[id];

                for (j, &sir_param_reg) in sir_block.params.iter().enumerate() {
                    let tval = if self.options.four_state {
                        let val = cl_params[j * 2];
                        let mask = cl_params[j * 2 + 1];
                        TransValue::FourState {
                            values: vec![val],
                            masks: vec![mask],
                        }
                    } else {
                        let val = cl_params[j];
                        TransValue::TwoState(vec![val])
                    };
                    state.regs.insert(sir_param_reg, tval);
                }

                for inst in &sir_block.instructions {
                    self.translate_instruction(&mut state, inst);
                }

                // For the last block of the last EU, use tail-call-aware terminator
                let use_tail_call = is_last_unit
                    && tail_call_info.is_some()
                    && matches!(sir_block.terminator, crate::ir::SIRTerminator::Return);

                if use_tail_call {
                    let info = tail_call_info.as_ref().unwrap();
                    self.emit_tail_call(&mut state, info);
                } else {
                    self.translate_terminator(
                        &mut state,
                        &sir_block.terminator,
                        &block_map,
                        next_unit_entry,
                    );
                }
            }
        }

        builder.seal_all_blocks();
        builder.finalize();
    }
}

impl SIRTranslator {
    /// Translate a single memory-spilled chunk.
    ///
    /// Each chunk is compiled as a separate function with signature `(mem_ptr) -> i64`.
    /// At entry, incoming live registers are loaded from scratch memory.
    /// At cross-chunk edges, outgoing registers are stored to scratch and a tail-call
    /// is emitted to the target chunk function.
    ///
    /// - `chunk`: the spilled chunk being compiled
    /// - `chunk_func_refs`: FuncRef for each chunk (indexed by chunk index)
    /// - `scratch_base_offset`: byte offset of the scratch region within unified memory
    pub fn translate_spilled_chunk(
        &self,
        chunk: &SpilledChunk,
        chunk_func_refs: &[FuncRef],
        scratch_base_offset: usize,
        mut builder: FunctionBuilder,
    ) {
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);

        let mem_ptr = builder.block_params(entry)[0];

        // Load incoming spills from scratch memory
        let mut spill_reg_values: HashMap<RegisterId, TransValue> = HashMap::default();
        for slot in &chunk.incoming_spills {
            let width = slot.reg_ty.width();
            let nc = width.div_ceil(64).max(1);

            if nc == 1 {
                let cl_ty = get_cl_type(width);
                let addr = builder.ins().iadd_imm(
                    mem_ptr,
                    (scratch_base_offset + slot.scratch_byte_offset) as i64,
                );
                let val = builder.ins().load(cl_ty, MemFlags::new(), addr, 0);
                if self.options.four_state {
                    let mask_addr = builder.ins().iadd_imm(
                        mem_ptr,
                        (scratch_base_offset + slot.scratch_byte_offset + 8) as i64,
                    );
                    let mask = builder.ins().load(cl_ty, MemFlags::new(), mask_addr, 0);
                    spill_reg_values.insert(
                        slot.reg_id,
                        TransValue::FourState {
                            values: vec![val],
                            masks: vec![mask],
                        },
                    );
                } else {
                    spill_reg_values.insert(slot.reg_id, TransValue::TwoState(vec![val]));
                }
            } else {
                // Wide register: load nc i64 chunks
                let mut values = Vec::with_capacity(nc);
                for i in 0..nc {
                    let off = scratch_base_offset + slot.scratch_byte_offset + i * 8;
                    let addr = builder.ins().iadd_imm(mem_ptr, off as i64);
                    let val = builder.ins().load(types::I64, MemFlags::new(), addr, 0);
                    values.push(val);
                }
                if self.options.four_state {
                    let mut masks = Vec::with_capacity(nc);
                    for i in 0..nc {
                        let off = scratch_base_offset + slot.scratch_byte_offset + (nc + i) * 8;
                        let addr = builder.ins().iadd_imm(mem_ptr, off as i64);
                        let val = builder.ins().load(types::I64, MemFlags::new(), addr, 0);
                        masks.push(val);
                    }
                    spill_reg_values.insert(slot.reg_id, TransValue::FourState { values, masks });
                } else {
                    spill_reg_values.insert(slot.reg_id, TransValue::TwoState(values));
                }
            }
        }

        let trigger_old_values = preload_trigger_old_values(
            chunk.eu.blocks.values(),
            &mut builder,
            mem_ptr,
            &self.layout,
            self.options.emit_triggers,
        );

        // Create block map for this chunk's EU
        let eu = &chunk.eu;
        let unit_entry = builder.create_block();
        let mut block_map: HashMap<BlockId, Block> = HashMap::default();

        // First pass: create all blocks
        for (id, block) in &eu.blocks {
            let cl_bb = if *id == eu.entry_block_id {
                unit_entry
            } else {
                builder.create_block()
            };
            for &param_reg in &block.params {
                let width = eu.register_map[&param_reg].width();
                let ty = get_cl_type(width);
                builder.append_block_param(cl_bb, ty);
                if self.options.four_state {
                    builder.append_block_param(cl_bb, ty);
                }
            }
            block_map.insert(*id, cl_bb);
        }

        // Jump from entry to the EU's entry block, passing spill values
        // for any block params the entry block expects.
        let entry_sir_block = &eu.blocks[&eu.entry_block_id];
        let mut entry_args: Vec<BlockArg> = Vec::new();
        for &param_reg in &entry_sir_block.params {
            if let Some(tv) = spill_reg_values.get(&param_reg) {
                match tv {
                    TransValue::TwoState(vals) => entry_args.push(BlockArg::Value(vals[0])),
                    TransValue::FourState { values, masks } => {
                        entry_args.push(BlockArg::Value(values[0]));
                        entry_args.push(BlockArg::Value(masks[0]));
                    }
                    TransValue::MemBacked { .. } => {
                        // Spill values are loaded from scratch — never MemBacked
                        unreachable!("spill_reg_values should never contain MemBacked")
                    }
                }
            } else {
                let width = eu.register_map[&param_reg].width();
                let ty = get_cl_type(width);
                let zero = builder.ins().iconst(ty, 0);
                entry_args.push(BlockArg::Value(zero));
                if self.options.four_state {
                    entry_args.push(BlockArg::Value(zero));
                }
            }
        }
        builder.ins().jump(unit_entry, &entry_args);

        // Derive cross-chunk targets from the edge map
        let cross_chunk_targets: HashMap<BlockId, usize> = chunk
            .cross_chunk_edges
            .iter()
            .map(|(&bid, edge)| (bid, edge.target_chunk_index))
            .collect();
        let cross_chunk_edges = &chunk.cross_chunk_edges;

        // Topological sort within the chunk so definitions precede uses.
        // (BlockId order worked for the original EU but not after partition_with_single_entry
        //  which can place high-numbered entry blocks before low-numbered successors.)
        let block_ids = crate::optimizer::coalescing::pass_tail_call_split::topological_sort_blocks(
            &eu.blocks,
            eu.entry_block_id,
        );

        // Create state ONCE for the entire chunk — SIR uses a flat register
        // space per EU, not strict SSA block params.
        let mut state = TranslationState {
            builder: &mut builder,
            regs: HashMap::default(),
            mem_ptr,
            register_map: &eu.register_map,
            trigger_old_values: trigger_old_values.clone(),
            imm_constants: HashMap::default(),
        };

        // Seed spill values (available in all blocks via dominating entry)
        for (reg_id, trans_val) in &spill_reg_values {
            state.regs.insert(*reg_id, trans_val.clone());
        }

        for id in &block_ids {
            let cl_block = block_map[id];
            state.builder.switch_to_block(cl_block);
            let cl_params: Vec<Value> = state.builder.block_params(cl_block).to_vec();
            let sir_block = &eu.blocks[id];

            // Map block params
            for (i, &sir_param_reg) in sir_block.params.iter().enumerate() {
                let tval = if self.options.four_state {
                    let val = cl_params[i * 2];
                    let mask = cl_params[i * 2 + 1];
                    TransValue::FourState {
                        values: vec![val],
                        masks: vec![mask],
                    }
                } else {
                    let val = cl_params[i];
                    TransValue::TwoState(vec![val])
                };
                state.regs.insert(sir_param_reg, tval);
            }

            // Translate instructions
            for inst in &sir_block.instructions {
                self.translate_instruction(&mut state, inst);
            }

            // Translate terminator with cross-chunk awareness
            self.translate_spilled_terminator(
                &mut state,
                &sir_block.terminator,
                &block_map,
                &cross_chunk_targets,
                cross_chunk_edges,
                chunk_func_refs,
                &chunk.outgoing_spills,
                scratch_base_offset,
            );
        }

        builder.seal_all_blocks();
        builder.finalize();
    }

    /// Translate a terminator for a spilled chunk. Local targets use normal
    /// jump/branch. Cross-chunk targets spill registers and tail-call.
    fn translate_spilled_terminator(
        &self,
        state: &mut TranslationState,
        term: &crate::ir::SIRTerminator,
        block_map: &HashMap<BlockId, Block>,
        cross_chunk_targets: &HashMap<BlockId, usize>,
        cross_chunk_edges: &HashMap<
            BlockId,
            crate::optimizer::coalescing::pass_tail_call_split::CrossChunkEdge,
        >,
        chunk_func_refs: &[FuncRef],
        outgoing_spills: &[SpillSlot],
        scratch_base_offset: usize,
    ) {
        match term {
            crate::ir::SIRTerminator::Jump(target, params) => {
                if let Some(&chunk_idx) = cross_chunk_targets.get(target) {
                    // Cross-chunk: spill + tail-call
                    self.emit_spill_and_tail_call(
                        state,
                        outgoing_spills,
                        scratch_base_offset,
                        chunk_func_refs[chunk_idx],
                        // Also spill the jump args into the target block's param scratch slots
                        cross_chunk_edges.get(target),
                        params,
                    );
                } else {
                    // Local jump — delegate to normal terminator translation
                    self.translate_terminator(state, term, block_map, None);
                }
            }
            crate::ir::SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                let t_cross = cross_chunk_targets.get(&true_block.0);
                let f_cross = cross_chunk_targets.get(&false_block.0);

                match (t_cross, f_cross) {
                    (None, None) => {
                        // Both local — delegate to normal terminator translation
                        self.translate_terminator(state, term, block_map, None);
                    }
                    (Some(&t_chunk), Some(&f_chunk)) => {
                        // Both cross-chunk: emit a brif to two trampoline blocks
                        let condition = state.regs[cond].first_value(state.builder);
                        let true_trampoline = state.builder.create_block();
                        let false_trampoline = state.builder.create_block();

                        state.builder.ins().brif(
                            condition,
                            true_trampoline,
                            &[],
                            false_trampoline,
                            &[],
                        );

                        state.builder.switch_to_block(true_trampoline);
                        self.emit_spill_and_tail_call(
                            state,
                            outgoing_spills,
                            scratch_base_offset,
                            chunk_func_refs[t_chunk],
                            cross_chunk_edges.get(&true_block.0),
                            &true_block.1,
                        );

                        state.builder.switch_to_block(false_trampoline);
                        self.emit_spill_and_tail_call(
                            state,
                            outgoing_spills,
                            scratch_base_offset,
                            chunk_func_refs[f_chunk],
                            cross_chunk_edges.get(&false_block.0),
                            &false_block.1,
                        );
                    }
                    (Some(&t_chunk), None) => {
                        // True is cross-chunk, false is local
                        let condition = state.regs[cond].first_value(state.builder);
                        let true_trampoline = state.builder.create_block();
                        let f_target = block_map[&false_block.0];
                        let cl_f_args =
                            self.build_local_block_args(state, f_target, &false_block.1);

                        state.builder.ins().brif(
                            condition,
                            true_trampoline,
                            &[],
                            f_target,
                            &cl_f_args,
                        );

                        state.builder.switch_to_block(true_trampoline);
                        self.emit_spill_and_tail_call(
                            state,
                            outgoing_spills,
                            scratch_base_offset,
                            chunk_func_refs[t_chunk],
                            cross_chunk_edges.get(&true_block.0),
                            &true_block.1,
                        );
                    }
                    (None, Some(&f_chunk)) => {
                        // True is local, false is cross-chunk
                        let condition = state.regs[cond].first_value(state.builder);
                        let false_trampoline = state.builder.create_block();
                        let t_target = block_map[&true_block.0];
                        let cl_t_args = self.build_local_block_args(state, t_target, &true_block.1);

                        state.builder.ins().brif(
                            condition,
                            t_target,
                            &cl_t_args,
                            false_trampoline,
                            &[],
                        );

                        state.builder.switch_to_block(false_trampoline);
                        self.emit_spill_and_tail_call(
                            state,
                            outgoing_spills,
                            scratch_base_offset,
                            chunk_func_refs[f_chunk],
                            cross_chunk_edges.get(&false_block.0),
                            &false_block.1,
                        );
                    }
                }
            }
            crate::ir::SIRTerminator::Return => {
                let success = state.builder.ins().iconst(types::I64, 0);
                state.builder.ins().return_(&[success]);
            }
            crate::ir::SIRTerminator::Error(code) => {
                let error = state.builder.ins().iconst(types::I64, *code);
                state.builder.ins().return_(&[error]);
            }
        }
    }

    /// Store outgoing spills to scratch memory and emit a tail call to the target chunk.
    fn emit_spill_and_tail_call(
        &self,
        state: &mut TranslationState,
        outgoing_spills: &[SpillSlot],
        scratch_base_offset: usize,
        target_func_ref: FuncRef,
        cross_chunk_edge: Option<
            &crate::optimizer::coalescing::pass_tail_call_split::CrossChunkEdge,
        >,
        jump_args: &[RegisterId],
    ) {
        // 1. Store outgoing live registers to scratch
        for slot in outgoing_spills {
            if let Some(trans_val) = state.regs.get(&slot.reg_id).cloned() {
                let values = trans_val.load_value_chunks(state.builder);
                for (i, &val) in values.iter().enumerate() {
                    let off = scratch_base_offset + slot.scratch_byte_offset + i * 8;
                    let addr = state.builder.ins().iadd_imm(state.mem_ptr, off as i64);
                    let val_i64 = cast_type(state.builder, val, types::I64);
                    state.builder.ins().store(MemFlags::new(), val_i64, addr, 0);
                }
                if self.options.four_state {
                    if let Some(masks) = trans_val.load_mask_chunks(state.builder) {
                        let nc = values.len();
                        for (i, &mask) in masks.iter().enumerate() {
                            let off = scratch_base_offset + slot.scratch_byte_offset + (nc + i) * 8;
                            let addr = state.builder.ins().iadd_imm(state.mem_ptr, off as i64);
                            let mask_i64 = cast_type(state.builder, mask, types::I64);
                            state
                                .builder
                                .ins()
                                .store(MemFlags::new(), mask_i64, addr, 0);
                        }
                    }
                }
            }
        }

        // 2. Store jump args into the target block's param scratch slots
        if let Some(edge) = cross_chunk_edge {
            for (i, &arg_reg) in jump_args.iter().enumerate() {
                if i < edge.param_scratch_offsets.len() {
                    let (_param_reg, scratch_off) = edge.param_scratch_offsets[i];
                    if let Some(trans_val) = state.regs.get(&arg_reg).cloned() {
                        let values = trans_val.load_value_chunks(state.builder);
                        for (j, &val) in values.iter().enumerate() {
                            let off = scratch_base_offset + scratch_off + j * 8;
                            let addr = state.builder.ins().iadd_imm(state.mem_ptr, off as i64);
                            let val_i64 = cast_type(state.builder, val, types::I64);
                            state.builder.ins().store(MemFlags::new(), val_i64, addr, 0);
                        }
                        if self.options.four_state {
                            if let Some(masks) = trans_val.load_mask_chunks(state.builder) {
                                let nc = values.len();
                                for (j, &mask) in masks.iter().enumerate() {
                                    let off = scratch_base_offset + scratch_off + (nc + j) * 8;
                                    let addr =
                                        state.builder.ins().iadd_imm(state.mem_ptr, off as i64);
                                    let mask_i64 = cast_type(state.builder, mask, types::I64);
                                    state
                                        .builder
                                        .ins()
                                        .store(MemFlags::new(), mask_i64, addr, 0);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 3. Tail-call to target chunk
        state
            .builder
            .ins()
            .return_call(target_func_ref, &[state.mem_ptr]);
    }

    /// Build block-call arguments for a local (non-cross-chunk) branch target.
    /// Collects values from state.regs and casts them to match the target block's
    /// declared parameter types.
    fn build_local_block_args(
        &self,
        state: &mut TranslationState,
        target: Block,
        args: &[RegisterId],
    ) -> Vec<BlockArg> {
        let param_types = collect_block_param_types(state, target);
        let mut cl_args: Vec<BlockArg> = Vec::new();
        for (i, reg) in args.iter().enumerate() {
            let val = state.regs[reg].first_value(state.builder);
            let val_idx = if self.options.four_state { i * 2 } else { i };
            let cast_val = cast_type(state.builder, val, param_types[val_idx]);
            cl_args.push(BlockArg::Value(cast_val));
            if self.options.four_state {
                let mask = state.regs[reg]
                    .first_mask(state.builder)
                    .unwrap_or_else(|| state.builder.ins().iconst(types::I8, 0));
                let cast_mask = cast_type(state.builder, mask, param_types[i * 2 + 1]);
                cl_args.push(BlockArg::Value(cast_mask));
            }
        }
        cl_args
    }

    /// Emit a tail call to the next chunk function.
    fn emit_tail_call(&self, state: &mut TranslationState, info: &TailCallInfo) {
        let mut args: Vec<Value> = vec![info.mem_ptr];
        for (reg_id, reg_ty) in &info.outgoing_live_regs {
            let width = reg_ty.width();
            let nc = width.div_ceil(64).max(1);
            // When nc==1, the signature uses get_cl_type(width) which may be I8/I16/I32.
            // When nc>1, each chunk is I64.
            let expected_ty = if nc == 1 {
                get_cl_type(width)
            } else {
                types::I64
            };
            if let Some(trans_val) = state.regs.get(reg_id).cloned() {
                let values = trans_val.load_value_chunks(state.builder);
                if self.options.four_state {
                    let masks_opt = trans_val.load_mask_chunks(state.builder);
                    for i in 0..nc {
                        let val = values
                            .get(i)
                            .copied()
                            .unwrap_or_else(|| state.builder.ins().iconst(expected_ty, 0));
                        args.push(cast_type(state.builder, val, expected_ty));
                        let mask = masks_opt
                            .as_ref()
                            .and_then(|m| m.get(i).copied())
                            .unwrap_or_else(|| state.builder.ins().iconst(expected_ty, 0));
                        args.push(cast_type(state.builder, mask, expected_ty));
                    }
                } else {
                    for i in 0..nc {
                        let val = values
                            .get(i)
                            .copied()
                            .unwrap_or_else(|| state.builder.ins().iconst(expected_ty, 0));
                        args.push(cast_type(state.builder, val, expected_ty));
                    }
                }
            } else {
                // Register not in scope — use zeros
                for _ in 0..nc {
                    args.push(state.builder.ins().iconst(expected_ty, 0));
                    if self.options.four_state {
                        args.push(state.builder.ins().iconst(expected_ty, 0));
                    }
                }
            }
        }
        state.builder.ins().return_call(info.func_ref, &args);
    }
}

/// Information needed to emit a tail call to the next chunk.
pub struct TailCallInfo {
    pub func_ref: FuncRef,
    pub mem_ptr: Value,
    pub outgoing_live_regs: Vec<(RegisterId, RegisterType)>,
}
