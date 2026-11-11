//! Block terminators and trigger detection.

use super::*;

// ============================================================
// Terminators
// ============================================================

pub(super) fn compile_terminator(
    term: &SIRTerminator,
    block_index: &HashMap<BlockId, usize>,
    _num_blocks: usize,
    block_id_local: u32,
    br_dispatch_depth: u32,
    br_exit_depth: u32,
    _unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    _four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    match term {
        SIRTerminator::Jump(target, args) => {
            // Copy args to block argument passing locals
            for (i, reg) in args.iter().enumerate() {
                let src = &locals.reg_map[reg];
                let passing = &locals.block_arg_locals[&(*target, i)];
                for c in 0..passing.num_chunks {
                    emit_wide_get_chunk(instrs, src, c);
                    instrs.push(Instruction::LocalSet(passing.value_idx + c as u32));
                }
                if let (Some(sm), Some(pm)) = (src.mask_idx, passing.mask_idx) {
                    for c in 0..passing.num_chunks {
                        instrs.push(Instruction::LocalGet(sm + c as u32));
                        instrs.push(Instruction::LocalSet(pm + c as u32));
                    }
                }
            }
            // Set block_id and branch to dispatch
            let target_idx = block_index[target];
            instrs.push(Instruction::I64Const(target_idx as i64));
            instrs.push(Instruction::LocalSet(block_id_local));
            // br $dispatch — which is at depth (num_blocks - current_block_index)
            // In our layout, $dispatch (loop) is at depth = num_blocks - block_index_of_current
            // Actually, after all block `end`s, we're inside the loop.
            // The loop label is always num_blocks levels up from the innermost block.
            // But we're currently *after* closing a block, inside the loop body.
            // Actually, the br targets are:
            //   - After the last block's end, we're directly inside the loop.
            //   - br 0 = continue loop ($dispatch)
            //   - br 1 = exit block ($exit)
            // So we always use br 0 to re-dispatch.
            instrs.push(Instruction::Br(br_dispatch_depth)); // br $dispatch (loop continue)
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let (t_id, t_args) = true_block;
            let (f_id, f_args) = false_block;

            // if (cond) { copy t_args; set block_id = t } else { copy f_args; set block_id = f }
            let cond_reg = &locals.reg_map[cond];
            instrs.push(Instruction::LocalGet(cond_reg.value_idx));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));

            // True branch
            for (i, reg) in t_args.iter().enumerate() {
                let src = &locals.reg_map[reg];
                let passing = &locals.block_arg_locals[&(*t_id, i)];
                for c in 0..passing.num_chunks {
                    emit_wide_get_chunk(instrs, src, c);
                    instrs.push(Instruction::LocalSet(passing.value_idx + c as u32));
                }
                if let (Some(sm), Some(pm)) = (src.mask_idx, passing.mask_idx) {
                    for c in 0..passing.num_chunks {
                        instrs.push(Instruction::LocalGet(sm + c as u32));
                        instrs.push(Instruction::LocalSet(pm + c as u32));
                    }
                }
            }
            instrs.push(Instruction::I64Const(block_index[t_id] as i64));
            instrs.push(Instruction::LocalSet(block_id_local));

            instrs.push(Instruction::Else);

            // False branch
            for (i, reg) in f_args.iter().enumerate() {
                let src = &locals.reg_map[reg];
                let passing = &locals.block_arg_locals[&(*f_id, i)];
                for c in 0..passing.num_chunks {
                    emit_wide_get_chunk(instrs, src, c);
                    instrs.push(Instruction::LocalSet(passing.value_idx + c as u32));
                }
                if let (Some(sm), Some(pm)) = (src.mask_idx, passing.mask_idx) {
                    for c in 0..passing.num_chunks {
                        instrs.push(Instruction::LocalGet(sm + c as u32));
                        instrs.push(Instruction::LocalSet(pm + c as u32));
                    }
                }
            }
            instrs.push(Instruction::I64Const(block_index[f_id] as i64));
            instrs.push(Instruction::LocalSet(block_id_local));

            instrs.push(Instruction::End); // end if

            instrs.push(Instruction::Br(br_dispatch_depth)); // br $dispatch
        }
        SIRTerminator::Switch {
            selector,
            cases,
            default,
        } => {
            instrs.push(Instruction::I64Const(block_index[default] as i64));
            instrs.push(Instruction::LocalSet(block_id_local));
            let selector = &locals.reg_map[selector];
            for case in cases {
                let digits = case.value.to_u64_digits();
                let value = match digits.as_slice() {
                    [] => 0,
                    [value] => *value,
                    _ => unreachable!("verified switch key fits eight bits"),
                };
                instrs.push(Instruction::LocalGet(selector.value_idx));
                instrs.push(Instruction::I64Const(value as i64));
                instrs.push(Instruction::I64Eq);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::I64Const(block_index[&case.target] as i64));
                instrs.push(Instruction::LocalSet(block_id_local));
                instrs.push(Instruction::End);
            }
            instrs.push(Instruction::Br(br_dispatch_depth));
        }
        SIRTerminator::Return => {
            // Break out of dispatch loop and unit.
            instrs.push(Instruction::Br(br_exit_depth)); // br $exit
        }
        SIRTerminator::Error(code) => {
            instrs.push(Instruction::I64Const(*code));
            instrs.push(Instruction::Return);
        }
    }
}

pub(super) fn collect_trigger_addrs(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
) -> Vec<(AbsoluteAddr, u32)> {
    let mut addrs = HashSet::<(AbsoluteAddr, u32)>::default();
    for unit in units {
        for block in unit.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Store(addr, _, _, _, triggers, _) if !triggers.is_empty() => {
                        addrs.insert((addr.absolute_addr(), addr.region));
                    }
                    SIRInstruction::Commit(_, dst, _, _, triggers) if !triggers.is_empty() => {
                        addrs.insert((dst.absolute_addr(), dst.region));
                    }
                    _ => {}
                }
            }
        }
    }
    addrs.into_iter().collect()
}

pub(super) fn emit_trigger_detection(
    addr: &RegionedAbsoluteAddr,
    triggers: &[TriggerIdWithKind],
    layout: &MemoryLayout,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let abs = addr.absolute_addr();
    let base_offset = compute_byte_offset(layout, &abs, addr.region);

    // Load current value
    instrs.push(Instruction::I32Const(base_offset as i32));
    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));

    // Load old value
    if let Some(&old_local) = locals.trigger_locals.get(&(abs, addr.region)) {
        instrs.push(Instruction::LocalGet(old_local));
    } else {
        instrs.push(Instruction::I64Const(0));
    }

    // Compare: if different, set trigger bits
    instrs.push(Instruction::I64Ne);
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
    for trigger in triggers {
        let byte_idx = trigger.id / 8;
        let bit_idx = trigger.id % 8;
        let trig_offset = layout.triggered_bits_offset + byte_idx;

        // Load current trigger byte
        instrs.push(Instruction::I32Const(trig_offset as i32));
        // Load old byte
        instrs.push(Instruction::I32Const(trig_offset as i32));
        instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        // OR in the bit
        instrs.push(Instruction::I32Const(1 << bit_idx));
        instrs.push(Instruction::I32Or);
        // Store back
        instrs.push(Instruction::I32Store8(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    }
    instrs.push(Instruction::End); // end if
}
