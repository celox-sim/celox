//! Sparse-array commits.

use super::*;

pub(super) fn compile_sparse_chunk_copy(
    src_base: usize,
    dst_base: usize,
    byte_index: u32,
    byte_len: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let mut copied = 0usize;
    for bytes in [8usize, 4, 2, 1] {
        while copied + bytes <= byte_len {
            for base in [dst_base + copied, src_base + copied] {
                instrs.push(Instruction::I64Const(base as i64));
                instrs.push(Instruction::LocalGet(byte_index));
                instrs.push(Instruction::I64Add);
                instrs.push(Instruction::I32WrapI64);
            }
            let memarg = wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            };
            match bytes {
                8 => {
                    instrs.push(Instruction::I64Load(memarg));
                    instrs.push(Instruction::I64Store(memarg));
                }
                4 => {
                    instrs.push(Instruction::I32Load(memarg));
                    instrs.push(Instruction::I32Store(memarg));
                }
                2 => {
                    instrs.push(Instruction::I32Load16U(memarg));
                    instrs.push(Instruction::I32Store16(memarg));
                }
                1 => {
                    instrs.push(Instruction::I32Load8U(memarg));
                    instrs.push(Instruction::I32Store8(memarg));
                }
                _ => unreachable!(),
            }
            copied += bytes;
        }
    }
}

pub(super) fn compile_sparse_commit(
    src_addr: &RegionedAbsoluteAddr,
    dst_addr: &RegionedAbsoluteAddr,
    _op_width: usize,
    layout: &MemoryLayout,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    debug_assert_eq!(src_addr.region, SPARSE_WORKING_REGION);
    debug_assert_eq!(dst_addr.region, STABLE_REGION);
    let abs = src_addr.absolute_addr();
    let sparse = &layout.sparse_layouts[&abs];
    let byte_size = layout.plane_size(&abs);
    let src_base = layout.sparse_base_offset + layout.sparse_offsets[&abs];
    let dst_base = layout.offsets[&abs];
    let plane_count = if four_state && layout.is_4states[&abs] {
        2
    } else {
        1
    };
    let last_chunk = sparse.chunk_count.saturating_sub(1);
    let last_len = byte_size.saturating_sub(last_chunk * 8);
    let memarg = wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };

    let summary_bits = locals.alloc(1);
    let dirty_bits = locals.alloc(1);
    let word_index = locals.alloc(1);
    let chunk = locals.alloc(1);
    let byte_index = locals.alloc(1);

    for summary_index in 0..sparse.summary_word_count {
        let summary_addr = sparse.summary_words_offset + summary_index * 8;
        instrs.push(Instruction::I32Const(summary_addr as i32));
        instrs.push(Instruction::I64Load(memarg));
        instrs.push(Instruction::LocalSet(summary_bits));
        instrs.push(Instruction::I32Const(summary_addr as i32));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Store(memarg));

        instrs.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        instrs.push(Instruction::Loop(wasm_encoder::BlockType::Empty));
        instrs.push(Instruction::LocalGet(summary_bits));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::BrIf(1));

        instrs.push(Instruction::LocalGet(summary_bits));
        instrs.push(Instruction::I64Ctz);
        instrs.push(Instruction::I64Const((summary_index * 64) as i64));
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalSet(word_index));
        instrs.push(Instruction::I32Const(sparse.dirty_words_offset as i32));
        instrs.push(Instruction::LocalGet(word_index));
        instrs.push(Instruction::I64Const(8));
        instrs.push(Instruction::I64Mul);
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::I32Add);
        instrs.push(Instruction::I64Load(memarg));
        instrs.push(Instruction::LocalSet(dirty_bits));
        instrs.push(Instruction::I32Const(sparse.dirty_words_offset as i32));
        instrs.push(Instruction::LocalGet(word_index));
        instrs.push(Instruction::I64Const(8));
        instrs.push(Instruction::I64Mul);
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::I32Add);
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Store(memarg));

        instrs.push(Instruction::Block(wasm_encoder::BlockType::Empty));
        instrs.push(Instruction::Loop(wasm_encoder::BlockType::Empty));
        instrs.push(Instruction::LocalGet(dirty_bits));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::BrIf(1));
        instrs.push(Instruction::LocalGet(word_index));
        instrs.push(Instruction::I64Const(64));
        instrs.push(Instruction::I64Mul);
        instrs.push(Instruction::LocalGet(dirty_bits));
        instrs.push(Instruction::I64Ctz);
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalTee(chunk));
        instrs.push(Instruction::I64Const(8));
        instrs.push(Instruction::I64Mul);
        instrs.push(Instruction::LocalSet(byte_index));

        if last_len == 8 {
            for plane in 0..plane_count {
                let delta = plane * byte_size;
                compile_sparse_chunk_copy(
                    src_base + delta,
                    dst_base + delta,
                    byte_index,
                    8,
                    instrs,
                );
            }
        } else {
            instrs.push(Instruction::LocalGet(chunk));
            instrs.push(Instruction::I64Const(last_chunk as i64));
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            for plane in 0..plane_count {
                let delta = plane * byte_size;
                compile_sparse_chunk_copy(
                    src_base + delta,
                    dst_base + delta,
                    byte_index,
                    last_len,
                    instrs,
                );
            }
            instrs.push(Instruction::Else);
            for plane in 0..plane_count {
                let delta = plane * byte_size;
                compile_sparse_chunk_copy(
                    src_base + delta,
                    dst_base + delta,
                    byte_index,
                    8,
                    instrs,
                );
            }
            instrs.push(Instruction::End);
        }

        instrs.push(Instruction::LocalGet(dirty_bits));
        instrs.push(Instruction::LocalGet(dirty_bits));
        instrs.push(Instruction::I64Const(1));
        instrs.push(Instruction::I64Sub);
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalSet(dirty_bits));
        instrs.push(Instruction::Br(0));
        instrs.push(Instruction::End);
        instrs.push(Instruction::End);

        instrs.push(Instruction::LocalGet(summary_bits));
        instrs.push(Instruction::LocalGet(summary_bits));
        instrs.push(Instruction::I64Const(1));
        instrs.push(Instruction::I64Sub);
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalSet(summary_bits));
        instrs.push(Instruction::Br(0));
        instrs.push(Instruction::End);
        instrs.push(Instruction::End);
    }
}
