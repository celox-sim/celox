use cranelift::prelude::*;
use cranelift_frontend::FunctionBuilder;

use crate::{
    HashMap, SimulatorOptions,
    ir::{AbsoluteAddr, RegionedAbsoluteAddr, RegisterId, RegisterType, SIRInstruction, STABLE_REGION},
};

use super::MemoryLayout;

pub enum TransValue {
    TwoState(Vec<Value>),
    FourState {
        values: Vec<Value>,
        masks: Vec<Value>,
    },
}

impl TransValue {
    pub fn values(&self) -> &[Value] {
        match self {
            TransValue::TwoState(v) => v,
            TransValue::FourState { values, .. } => values,
        }
    }
    pub fn masks(&self) -> Option<&[Value]> {
        match self {
            TransValue::TwoState(_) => None,
            TransValue::FourState { masks, .. } => Some(masks),
        }
    }
}

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
    } else {
        if is_signed {
            state.builder.ins().sextend(dst_phys_ty, val)
        } else {
            state.builder.ins().uextend(dst_phys_ty, val)
        }
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

impl SIRTranslator {
    fn translate_instruction(
        &self,
        state: &mut TranslationState,
        inst: &SIRInstruction<RegionedAbsoluteAddr>,
    ) {
        match inst {
            SIRInstruction::Imm(dst, val) => {
                self.translate_imm_inst(state, dst, val);
            }
            SIRInstruction::Concat(dst, args) => {
                self.translate_concat_inst(state, dst, args);
            }
            SIRInstruction::Binary(dst, lhs, op, rhs) => {
                self.translate_binary_inst(state, dst, lhs, op, rhs);
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

        // Pre-load trigger signal values at function entry for register-based
        // edge detection. Only needed when emit_triggers is enabled (Simulation mode).
        let trigger_old_values = if !self.options.emit_triggers {
            HashMap::default()
        } else {
            let mut trigger_addrs: std::collections::HashSet<(AbsoluteAddr, u32)> =
                std::collections::HashSet::new();
            for unit in units {
                for block in unit.blocks.values() {
                    for inst in &block.instructions {
                        match inst {
                            SIRInstruction::Store(addr, _, _, _, triggers)
                                if !triggers.is_empty() =>
                            {
                                trigger_addrs.insert((addr.absolute_addr(), addr.region));
                            }
                            SIRInstruction::Commit(_, dst, _, _, triggers)
                                if !triggers.is_empty() =>
                            {
                                trigger_addrs.insert((dst.absolute_addr(), dst.region));
                            }
                            _ => {}
                        }
                    }
                }
            }

            let mut old_values: HashMap<(AbsoluteAddr, u32), Value> = HashMap::default();
            for (abs, region) in trigger_addrs {
                let width = self.layout.widths[&abs];
                debug_assert!(
                    width <= 64,
                    "Trigger signal wider than 64 bits is not supported"
                );
                let cl_type = get_cl_type(width);
                let base_offset = if region == STABLE_REGION {
                    self.layout.offsets[&abs]
                } else {
                    self.layout.working_base_offset + self.layout.working_offsets[&abs]
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
        };

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
}
