//! Runtime event records and combinational capture sites.

use super::*;

pub(super) fn compile_runtime_event(
    event_header_offset: usize,
    enabled_site: Option<(u32, bool)>,
    site_id: u32,
    args: &[RegisterId],
    fatal_error_code: Option<i64>,
    layout: &MemoryLayout,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let enabled_ptr = enabled_site.map(|(site, _)| {
        let ptr = locals.alloc(1);
        instrs.push(Instruction::I32Const(
            STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET as i32,
        ));
        instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        instrs.push(Instruction::LocalSet(ptr));
        instrs.push(Instruction::LocalGet(ptr));
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
            offset: site as u64,
            align: 0,
            memory_index: 0,
        }));
        instrs.push(Instruction::I32Const(0));
        instrs.push(Instruction::I32Ne);
        instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
        ptr
    });

    emit_runtime_event_write(event_header_offset, site_id, args, layout, locals, instrs);

    if let Some((site, consume_enabled)) = enabled_site {
        if consume_enabled {
            let ptr = enabled_ptr.expect("enabled pointer must be available");
            instrs.push(Instruction::LocalGet(ptr));
            instrs.push(Instruction::I32WrapI64);
            instrs.push(Instruction::I32Const(0));
            instrs.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: site as u64,
                align: 0,
                memory_index: 0,
            }));
        }
    }

    if let Some(code) = fatal_error_code {
        instrs.push(Instruction::I64Const(code));
        instrs.push(Instruction::Return);
    }
    if enabled_site.is_some() {
        instrs.push(Instruction::End);
    }
}

pub(super) fn emit_runtime_event_write(
    event_header_offset: usize,
    site_id: u32,
    args: &[RegisterId],
    layout: &MemoryLayout,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let event_ptr = locals.alloc(1);
    let seq = locals.alloc(1);
    let slot_addr = locals.alloc(1);

    instrs.push(Instruction::I32Const(event_header_offset as i32));
    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    instrs.push(Instruction::LocalSet(event_ptr));

    instrs.push(Instruction::LocalGet(event_ptr));
    instrs.push(Instruction::I32WrapI64);
    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    instrs.push(Instruction::LocalSet(seq));

    instrs.push(Instruction::LocalGet(event_ptr));
    instrs.push(Instruction::I64Const(RUNTIME_EVENT_HEADER_SIZE as i64));
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalGet(seq));
    instrs.push(Instruction::I64Const(
        (layout.runtime_event_capacity as i64) - 1,
    ));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::I64Const(layout.runtime_event_slot_size as i64));
    instrs.push(Instruction::I64Mul);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalSet(slot_addr));

    emit_i64_store_at_ptr_local(
        slot_addr,
        RUNTIME_EVENT_SLOT_SEQ_OFFSET,
        RUNTIME_EVENT_WRITING,
        instrs,
    );
    emit_i64_store_at_ptr_local(
        slot_addr,
        RUNTIME_EVENT_SLOT_SITE_OFFSET,
        site_id as u64,
        instrs,
    );
    emit_i64_store_at_ptr_local(
        slot_addr,
        RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
        args.len() as u64,
        instrs,
    );

    if let Some(site_layout) = layout.runtime_event_site_layouts.get(site_id as usize) {
        for (idx, arg) in args.iter().enumerate() {
            let Some(arg_layout) = site_layout.args.get(idx) else {
                continue;
            };
            let reg = &locals.reg_map[arg];
            for word_idx in 0..arg_layout.word_count {
                instrs.push(Instruction::LocalGet(slot_addr));
                instrs.push(Instruction::I32WrapI64);
                if word_idx < reg.num_chunks {
                    instrs.push(Instruction::LocalGet(reg.value_idx + word_idx as u32));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
                    offset: (RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                        + (arg_layout.value_word_offset + word_idx) * 8)
                        as u64,
                    align: 0,
                    memory_index: 0,
                }));

                instrs.push(Instruction::LocalGet(slot_addr));
                instrs.push(Instruction::I32WrapI64);
                if let Some(mask_idx) = reg.mask_idx {
                    if word_idx < reg.num_chunks {
                        instrs.push(Instruction::LocalGet(mask_idx + word_idx as u32));
                    } else {
                        instrs.push(Instruction::I64Const(0));
                    }
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
                    offset: (RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                        + (arg_layout.mask_word_offset + word_idx) * 8)
                        as u64,
                    align: 0,
                    memory_index: 0,
                }));
            }
        }
    }

    instrs.push(Instruction::LocalGet(slot_addr));
    instrs.push(Instruction::I32WrapI64);
    instrs.push(Instruction::LocalGet(seq));
    instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
        offset: RUNTIME_EVENT_SLOT_SEQ_OFFSET as u64,
        align: 0,
        memory_index: 0,
    }));

    instrs.push(Instruction::LocalGet(event_ptr));
    instrs.push(Instruction::I32WrapI64);
    instrs.push(Instruction::LocalGet(seq));
    instrs.push(Instruction::I64Const(1));
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
}

pub(super) fn emit_i64_store_at_ptr_local(
    ptr_local: u32,
    offset: usize,
    value: u64,
    instrs: &mut Vec<Instruction<'static>>,
) {
    instrs.push(Instruction::LocalGet(ptr_local));
    instrs.push(Instruction::I32WrapI64);
    instrs.push(Instruction::I64Const(value as i64));
    instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
        offset: offset as u64,
        align: 0,
        memory_index: 0,
    }));
}

pub(super) fn compile_comb_capture_enable_if_changed(
    old: &RegisterId,
    new: &RegisterId,
    sites: &[u32],
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    if sites.is_empty() {
        return;
    }
    let changed = locals.alloc(1);
    emit_regs_changed(old, new, four_state, locals, instrs);
    instrs.push(Instruction::I64ExtendI32U);
    instrs.push(Instruction::LocalSet(changed));
    emit_enable_comb_capture_sites(changed, sites, locals, instrs);
}

pub(super) fn emit_regs_changed(
    old: &RegisterId,
    new: &RegisterId,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let old_reg = &locals.reg_map[old];
    let new_reg = &locals.reg_map[new];
    let chunks = old_reg.num_chunks.max(new_reg.num_chunks);
    instrs.push(Instruction::I32Const(0));
    for c in 0..chunks {
        emit_wide_get_chunk(instrs, old_reg, c);
        emit_wide_get_chunk(instrs, new_reg, c);
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::I32Or);
    }
    if four_state {
        for c in 0..chunks {
            emit_mask_get_chunk(instrs, old_reg, c);
            emit_mask_get_chunk(instrs, new_reg, c);
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::I32Or);
        }
    }
}

pub(super) fn emit_mask_get_chunk(
    instrs: &mut Vec<Instruction<'static>>,
    reg: &RegLocal,
    chunk: usize,
) {
    if let Some(mask_idx) = reg.mask_idx
        && chunk < reg.num_chunks
    {
        instrs.push(Instruction::LocalGet(mask_idx + chunk as u32));
    } else {
        instrs.push(Instruction::I64Const(0));
    }
}

pub(super) fn emit_enable_comb_capture_sites(
    changed_local: u32,
    sites: &[u32],
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let enabled_ptr = locals.alloc(1);
    instrs.push(Instruction::I32Const(
        STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET as i32,
    ));
    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    instrs.push(Instruction::LocalSet(enabled_ptr));

    instrs.push(Instruction::LocalGet(changed_local));
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::I64Ne);
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
    for &site in sites {
        instrs.push(Instruction::LocalGet(enabled_ptr));
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::I32Const(1));
        instrs.push(Instruction::I32Store8(wasm_encoder::MemArg {
            offset: site as u64,
            align: 0,
            memory_index: 0,
        }));
    }
    instrs.push(Instruction::End);
}
