//! Loads, stores and masks of words narrower than a chunk.

use super::*;

// ============================================================
// Helpers
// ============================================================

pub(super) fn compute_byte_offset(layout: &MemoryLayout, abs: &AbsoluteAddr, region: u32) -> usize {
    match region {
        STABLE_REGION => layout.offsets[abs],
        SPARSE_WORKING_REGION => layout.sparse_base_offset + layout.sparse_offsets[abs],
        _ => layout.working_base_offset + layout.working_offsets[abs],
    }
}

pub(super) fn mask_for_bytes(bytes: usize) -> u64 {
    match bytes {
        0 => 0,
        8.. => u64::MAX,
        _ => (1u64 << (bytes * 8)) - 1,
    }
}

pub(super) fn emit_load_small_word(
    byte_offset: usize,
    byte_len: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    emit_load_small_word_based(Base::Absolute, byte_offset, byte_len, instrs)
}

pub(super) fn emit_load_small_word_based(
    base: Base,
    byte_offset: usize,
    byte_len: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    match byte_len {
        0 => instrs.push(Instruction::I64Const(0)),
        1 => {
            emit_address(base, byte_offset, instrs);
            instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        2 => {
            emit_address(base, byte_offset, instrs);
            instrs.push(Instruction::I32Load16U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        3 => {
            emit_load_small_word_based(base, byte_offset, 2, instrs);
            emit_load_small_word_based(base, byte_offset + 2, 1, instrs);
            instrs.push(Instruction::I64Const(16));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        4 => {
            emit_address(base, byte_offset, instrs);
            instrs.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        5 => {
            emit_load_small_word_based(base, byte_offset, 4, instrs);
            emit_load_small_word_based(base, byte_offset + 4, 1, instrs);
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        6 => {
            emit_load_small_word_based(base, byte_offset, 4, instrs);
            emit_load_small_word_based(base, byte_offset + 4, 2, instrs);
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        7 => {
            emit_load_small_word_based(base, byte_offset, 4, instrs);
            emit_load_small_word_based(base, byte_offset + 4, 2, instrs);
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
            emit_load_small_word_based(base, byte_offset + 6, 1, instrs);
            instrs.push(Instruction::I64Const(48));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        _ => {
            emit_address(base, byte_offset, instrs);
            instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
        }
    }
}

pub(super) fn emit_store_small_word_based(
    base: Base,
    byte_offset: usize,
    byte_len: usize,
    value_local: u32,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let memarg = wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    match byte_len {
        0 => {}
        1 => {
            emit_address(base, byte_offset, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store8(memarg));
        }
        2 => {
            emit_address(base, byte_offset, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store16(memarg));
        }
        3 => {
            emit_store_small_word_based(base, byte_offset, 2, value_local, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(16));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(value_local));
            emit_store_small_word_based(base, byte_offset + 2, 1, value_local, instrs);
        }
        4 => {
            emit_address(base, byte_offset, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store32(memarg));
        }
        5 => {
            emit_store_small_word_based(base, byte_offset, 4, value_local, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(value_local));
            emit_store_small_word_based(base, byte_offset + 4, 1, value_local, instrs);
        }
        6 => {
            emit_store_small_word_based(base, byte_offset, 4, value_local, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(value_local));
            emit_store_small_word_based(base, byte_offset + 4, 2, value_local, instrs);
        }
        7 => {
            emit_store_small_word_based(base, byte_offset, 4, value_local, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(value_local));
            emit_store_small_word_based(base, byte_offset + 4, 2, value_local, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(16));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(value_local));
            emit_store_small_word_based(base, byte_offset + 6, 1, value_local, instrs);
        }
        _ => {
            emit_address(base, byte_offset, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store(memarg));
        }
    }
}

pub(super) fn emit_mask_to_width(instrs: &mut Vec<Instruction<'static>>, width: usize) {
    if width > 0 && width < 64 {
        let mask = (1u64 << width) - 1;
        instrs.push(Instruction::I64Const(mask as i64));
        instrs.push(Instruction::I64And);
    }
}

pub(super) fn chunk_mask_for_width(chunk: usize, num_chunks: usize, width: usize) -> u64 {
    if chunk + 1 < num_chunks {
        u64::MAX
    } else {
        let top_bits = width % 64;
        if top_bits == 0 {
            u64::MAX
        } else {
            (1u64 << top_bits) - 1
        }
    }
}

pub(super) fn emit_chunk_mask_to_width(
    instrs: &mut Vec<Instruction<'static>>,
    chunk: usize,
    num_chunks: usize,
    width: usize,
) {
    let mask = chunk_mask_for_width(chunk, num_chunks, width);
    if mask != u64::MAX {
        instrs.push(Instruction::I64Const(mask as i64));
        instrs.push(Instruction::I64And);
    }
}

// Saturate SAR's unsigned count; WebAssembly masks an i64 shift to six bits.
pub(super) fn emit_sar_count(instrs: &mut Vec<Instruction<'static>>, count: u32) {
    instrs.push(Instruction::LocalGet(count));
    instrs.push(Instruction::I64Const(63));
    instrs.push(Instruction::LocalGet(count));
    instrs.push(Instruction::I64Const(63));
    instrs.push(Instruction::I64LtU);
    instrs.push(Instruction::Select);
}

/// Replaces the shifted value on the stack with zero when the logical shift
/// count is at least 64. Wasm shifts use the count modulo 64.
pub(super) fn emit_logical_shift_saturation(instrs: &mut Vec<Instruction<'static>>, count: u32) {
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalGet(count));
    instrs.push(Instruction::I64Const(64));
    instrs.push(Instruction::I64LtU);
    instrs.push(Instruction::Select);
}

pub(super) fn emit_sign_extend(
    instrs: &mut Vec<Instruction<'static>>,
    local_idx: u32,
    width: usize,
) {
    instrs.push(Instruction::LocalGet(local_idx));
    if width < 64 {
        let shift = 64 - width;
        instrs.push(Instruction::I64Const(shift as i64));
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Const(shift as i64));
        instrs.push(Instruction::I64ShrS);
    }
}
