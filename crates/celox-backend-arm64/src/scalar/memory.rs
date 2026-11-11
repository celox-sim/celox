//! Block memory copies and fills, and sparse commit emission.

use super::*;

pub(super) fn emit_mem_copy(
    ops: &mut VecAssembler<Aarch64Relocation>,
    src_offset: i32,
    dst_offset: i32,
    byte_len: usize,
    state_pages: StatePageBases,
) {
    if byte_len == 0 || src_offset == dst_offset {
        return;
    }
    if mem_copy_uses_forward_vectors(src_offset, dst_offset, byte_len) {
        emit_mem_copy_forward_vectors(ops, src_offset, dst_offset, byte_len, state_pages);
        return;
    }
    let src_end = i64::from(src_offset) + byte_len as i64;
    let dst_end = i64::from(dst_offset) + byte_len as i64;
    let copy_backward = src_end > i64::from(dst_offset)
        && dst_end > i64::from(src_offset)
        && dst_offset > src_offset;
    let qwords = byte_len / 8;
    let remainder = byte_len % 8;

    if copy_backward {
        emit_load_imm(ops, SCRATCH0, src_end as u64);
        dynasm!(ops ; .arch aarch64 ; add x16, x0, x16);
        emit_load_imm(ops, SCRATCH1, dst_end as u64);
        dynasm!(ops ; .arch aarch64 ; add x17, x0, x17);
        if remainder >= 4 {
            dynasm!(ops
                ; .arch aarch64
                ; ldr w30, [x16, #-4]!
                ; str w30, [x17, #-4]!
            );
        }
        if remainder % 4 >= 2 {
            dynasm!(ops
                ; .arch aarch64
                ; ldrh w30, [x16, #-2]!
                ; strh w30, [x17, #-2]!
            );
        }
        if remainder % 2 == 1 {
            dynasm!(ops
                ; .arch aarch64
                ; ldrb w30, [x16, #-1]!
                ; strb w30, [x17, #-1]!
            );
        }
        if qwords != 0 {
            let loop_label = ops.new_dynamic_label();
            let done = ops.new_dynamic_label();
            emit_load_imm(ops, 30, dst_offset as i64 as u64);
            dynasm!(ops
                ; .arch aarch64
                ; add x30, x0, x30
                ; =>loop_label
                ; cmp x17, x30
                ; b.ls =>done
                ; ldr d0, [x16, #-8]!
                ; str d0, [x17, #-8]!
                ; b =>loop_label
                ; =>done
            );
        }
        return;
    }

    emit_load_imm(ops, SCRATCH0, src_offset as i64 as u64);
    dynasm!(ops ; .arch aarch64 ; add x16, x0, x16);
    emit_load_imm(ops, SCRATCH1, dst_offset as i64 as u64);
    dynasm!(ops ; .arch aarch64 ; add x17, x0, x17);
    if qwords != 0 {
        let loop_label = ops.new_dynamic_label();
        let done = ops.new_dynamic_label();
        emit_load_imm(
            ops,
            30,
            (i64::from(dst_offset) + (qwords * 8) as i64) as u64,
        );
        dynasm!(ops
            ; .arch aarch64
            ; add x30, x0, x30
            ; =>loop_label
            ; cmp x17, x30
            ; b.hs =>done
            ; ldr d0, [x16], #8
            ; str d0, [x17], #8
            ; b =>loop_label
            ; =>done
        );
    }
    if remainder >= 4 {
        dynasm!(ops ; .arch aarch64 ; ldr w30, [x16], #4 ; str w30, [x17], #4);
    }
    if remainder % 4 >= 2 {
        dynasm!(ops ; .arch aarch64 ; ldrh w30, [x16], #2 ; strh w30, [x17], #2);
    }
    if remainder % 2 == 1 {
        dynasm!(ops ; .arch aarch64 ; ldrb w30, [x16] ; strb w30, [x17]);
    }
}

pub(super) fn emit_mem_copy_forward_vectors(
    ops: &mut VecAssembler<Aarch64Relocation>,
    src_offset: i32,
    dst_offset: i32,
    byte_len: usize,
    state_pages: StatePageBases,
) {
    debug_assert!(byte_len <= 256);
    let (src_base, src_relative) =
        select_vector_memory_base(BaseReg::SimState, i64::from(src_offset), state_pages);
    emit_address_to(ops, SCRATCH0, src_base, src_relative);
    let (dst_base, dst_relative) =
        select_vector_memory_base(BaseReg::SimState, i64::from(dst_offset), state_pages);
    emit_address_to(ops, SCRATCH1, dst_base, dst_relative);

    // Fixed offsets avoid a dependent address update after every vector.
    // Pair adjacent vectors while keeping forward memmove ordering.
    let pairs = byte_len / 32;
    for pair in 0..pairs {
        let offset = (pair * 32) as i32;
        dynasm!(ops
            ; .arch aarch64
            ; ldp q0, q1, [x16, offset]
            ; stp q0, q1, [x17, offset]
        );
    }
    let mut offset = (pairs * 32) as u32;
    if byte_len % 32 >= 16 {
        dynasm!(ops ; .arch aarch64 ; ldr q0, [x16, offset] ; str q0, [x17, offset]);
        offset += 16;
    }
    let remainder = byte_len % 16;
    if remainder >= 8 {
        dynasm!(ops ; .arch aarch64 ; ldr x30, [x16, offset] ; str x30, [x17, offset]);
        offset += 8;
    }
    if remainder % 8 >= 4 {
        dynasm!(ops ; .arch aarch64 ; ldr w30, [x16, offset] ; str w30, [x17, offset]);
        offset += 4;
    }
    if remainder % 4 >= 2 {
        dynasm!(ops ; .arch aarch64 ; ldrh w30, [x16, offset] ; strh w30, [x17, offset]);
        offset += 2;
    }
    if remainder % 2 == 1 {
        dynasm!(ops ; .arch aarch64 ; ldrb w30, [x16, offset] ; strb w30, [x17, offset]);
    }
}

pub(super) fn emit_sparse_commit_worklist(
    ops: &mut VecAssembler<Aarch64Relocation>,
    descriptors: &[u64],
    active_bits_offset: i32,
    active_capacity: usize,
    state_pages: StatePageBases,
) -> Result<(), EmitError> {
    for word_index in 0..active_capacity.div_ceil(64) {
        let word_offset = i32::try_from(word_index * 8)
            .map_err(|_| EmitError::Range("sparse active bitmap offset exceeds i32"))?;
        let offset = active_bits_offset
            .checked_add(word_offset)
            .ok_or(EmitError::Range("sparse active bitmap offset overflow"))?;
        let word_done = ops.new_dynamic_label();
        emit_sparse_bitmap_take(ops, i64::from(offset), state_pages);
        dynasm!(ops
            ; .arch aarch64
            ; cbz x17, =>word_done
        );
        let first_index = word_index * 64;
        let end_index = active_capacity.min(first_index + 64);
        let mut group_done = None;
        for active_index in first_index..end_index {
            if active_index % 8 == 0 {
                let done = ops.new_dynamic_label();
                let mask = 0xff_u64 << (active_index % 64);
                // Sparse words commonly contain only one or two entries.
                // Skip eight absent entries with one test and branch.
                ops.push_u32(
                    logical_immediate_encoding(mask, 64, 31, SCRATCH1, true).unwrap() | (3 << 29),
                );
                dynasm!(ops ; .arch aarch64 ; b.eq =>done);
                group_done = Some(done);
            }
            let row_start = active_index
                .checked_mul(SPARSE_COMMIT_DESCRIPTOR_WORDS)
                .ok_or(EmitError::Range("sparse descriptor index overflow"))?;
            let row = descriptors
                .get(row_start..row_start + SPARSE_COMMIT_DESCRIPTOR_WORDS)
                .ok_or(EmitError::Range("sparse descriptor row is missing"))?;
            let skip = ops.new_dynamic_label();
            let bit = (active_index % 64) as u32;
            if row[6] <= 1 {
                // One summary word has a bounded, small body that fits
                // TBZ's +/-32 KiB displacement. Large arrays use B.cond.
                dynasm!(ops ; .arch aarch64 ; tbz x17, bit, =>skip);
            } else {
                let mask = 1_u64 << bit;
                // Logical-immediate opc=0b11 selects ANDS, whose xzr
                // destination encodes TST. The non-flag-setting opcodes
                // interpret destination 31 as SP instead.
                ops.push_u32(
                    logical_immediate_encoding(mask, 64, 31, SCRATCH1, true).unwrap() | (3 << 29),
                );
                dynasm!(ops ; .arch aarch64 ; b.eq =>skip);
            }
            // Remove the entry before saving the remaining work. Once its
            // commit returns, an empty word can skip the rest of the tests.
            ops.push_u32(
                logical_immediate_encoding(!(1_u64 << bit), 64, SCRATCH1, SCRATCH1, true).unwrap(),
            );
            dynasm!(ops ; .arch aarch64 ; fmov d5, x17);
            emit_sparse_commit(
                ops,
                i32::try_from(row[0])
                    .map_err(|_| EmitError::Range("sparse source offset exceeds i32"))?,
                i32::try_from(row[1])
                    .map_err(|_| EmitError::Range("sparse destination offset exceeds i32"))?,
                usize::try_from(row[2])
                    .map_err(|_| EmitError::Range("sparse byte size exceeds usize"))?,
                i32::try_from(row[3])
                    .map_err(|_| EmitError::Range("sparse dirty offset exceeds i32"))?,
                usize::try_from(row[4])
                    .map_err(|_| EmitError::Range("sparse dirty count exceeds usize"))?,
                i32::try_from(row[5])
                    .map_err(|_| EmitError::Range("sparse summary offset exceeds i32"))?,
                usize::try_from(row[6])
                    .map_err(|_| EmitError::Range("sparse summary count exceeds usize"))?,
                row[7] != 0,
                state_pages,
            );
            dynasm!(ops ; .arch aarch64 ; fmov x17, d5 ; cbz x17, =>word_done ; =>skip);
            if (active_index + 1) % 8 == 0 || active_index + 1 == end_index {
                let done = group_done.take().expect("each group has a skip label");
                dynasm!(ops ; .arch aarch64 ; =>done);
            }
        }
        dynasm!(ops ; .arch aarch64 ; =>word_done);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_sparse_commit(
    ops: &mut VecAssembler<Aarch64Relocation>,
    src_offset: i32,
    dst_offset: i32,
    byte_size: usize,
    dirty_words_offset: i32,
    dirty_word_count: usize,
    summary_words_offset: i32,
    summary_word_count: usize,
    four_state: bool,
    state_pages: StatePageBases,
) {
    if (1..=8).contains(&byte_size) && dirty_word_count == 1 && summary_word_count == 1 {
        // A single chunk needs only bit zero of each bitmap. Preserve the
        // general path's clearing and invalid-bit behavior, without either
        // bit-scan loop or its saved loop indices.
        let done = ops.new_dynamic_label();
        emit_sparse_bitmap_take(ops, i64::from(summary_words_offset), state_pages);
        dynasm!(ops
            ; .arch aarch64
            ; tbz x17, #0, =>done
        );
        emit_sparse_bitmap_take(ops, i64::from(dirty_words_offset), state_pages);
        dynasm!(ops
            ; .arch aarch64
            ; tbz x17, #0, =>done
        );
        for plane in 0..if four_state { 2 } else { 1 } {
            let delta = (plane * byte_size) as i32;
            emit_mem_copy_forward_vectors(
                ops,
                src_offset + delta,
                dst_offset + delta,
                byte_size,
                state_pages,
            );
        }
        dynasm!(ops ; .arch aarch64 ; =>done);
        return;
    }
    let chunk_count = byte_size.div_ceil(8);
    if (2..=64).contains(&chunk_count) && dirty_word_count == 1 && summary_word_count == 1 {
        let done = ops.new_dynamic_label();
        let dirty_loop = ops.new_dynamic_label();
        emit_sparse_bitmap_take(ops, i64::from(summary_words_offset), state_pages);
        dynasm!(ops ; .arch aarch64 ; tbz x17, #0, =>done);
        emit_sparse_bitmap_take(ops, i64::from(dirty_words_offset), state_pages);
        if chunk_count < 64 {
            // A single dirty word covers the whole array. Discard invalid
            // high bits once, so its loop needs no summary scan or bounds test.
            ops.push_u32(
                logical_immediate_encoding(
                    (1_u64 << chunk_count) - 1,
                    64,
                    SCRATCH1,
                    SCRATCH1,
                    true,
                )
                .unwrap(),
            );
        }
        dynasm!(ops
            ; .arch aarch64
            ; cbz x17, =>done
            ; =>dirty_loop
            ; rbit x16, x17
            ; clz x16, x16
            ; sub x30, x17, #1
            ; and x17, x17, x30
            ; fmov d2, x17
            ; lsl x17, x16, #3
        );
        emit_sparse_selected_chunk(
            ops,
            src_offset,
            dst_offset,
            byte_size,
            four_state,
            state_pages,
        );
        dynasm!(ops
            ; .arch aarch64
            ; fmov x17, d2
            ; cbnz x17, =>dirty_loop
            ; =>done
        );
        return;
    }

    for summary_index in 0..summary_word_count {
        let summary_loop = ops.new_dynamic_label();
        let summary_done = ops.new_dynamic_label();
        let summary_next = ops.new_dynamic_label();
        let dirty_loop = ops.new_dynamic_label();
        let dirty_restore = ops.new_dynamic_label();
        let summary_offset = i64::from(summary_words_offset) + (summary_index * 8) as i64;
        emit_sparse_bitmap_take(ops, summary_offset, state_pages);
        dynasm!(ops
            ; .arch aarch64
            ; mov x16, x17
            ; =>summary_loop
            ; cbz x16, =>summary_done
            ; rbit x17, x16
            ; clz x17, x17
            ; sub x30, x16, #1
            ; and x16, x16, x30
        );
        if summary_index != 0 {
            emit_load_imm(ops, 30, (summary_index * 64) as u64);
            dynasm!(ops ; .arch aarch64 ; add x17, x17, x30);
        }
        emit_load_imm(ops, 30, dirty_word_count as u64);
        dynasm!(ops
            ; .arch aarch64
            ; cmp x17, x30
            ; b.hs =>summary_loop
            ; fmov d0, x16
            ; fmov d1, x17
            ; lsl x17, x17, #3
        );
        emit_sparse_indexed_address(ops, SCRATCH0, dirty_words_offset, state_pages);
        dynasm!(ops
            ; .arch aarch64
            ; ldr x30, [x16]
            ; str xzr, [x16]
            ; =>dirty_loop
            ; cbz x30, =>summary_next
            ; rbit x17, x30
            ; clz x17, x17
            ; sub x16, x30, #1
            ; and x30, x30, x16
            ; fmov d2, x30
            ; fmov x16, d1
            ; lsl x16, x16, #6
            ; add x17, x16, x17
        );
        emit_load_imm(ops, SCRATCH0, chunk_count as u64);
        dynasm!(ops
            ; .arch aarch64
            ; cmp x17, x16
            ; b.hs =>dirty_restore
            ; lsl x17, x17, #3
        );
        emit_sparse_selected_chunk(
            ops,
            src_offset,
            dst_offset,
            byte_size,
            four_state,
            state_pages,
        );
        dynasm!(ops
            ; .arch aarch64
            ; =>dirty_restore
            ; fmov x30, d2
            ; b =>dirty_loop
            ; =>summary_next
            ; fmov x16, d0
            ; b =>summary_loop
            ; =>summary_done
        );
    }
}

/// Copy the chunk at byte index x17, including a partial final chunk and both
/// value planes when present. Preserve the enclosing worklists in d0..d2/d5.
pub(super) fn emit_sparse_selected_chunk(
    ops: &mut VecAssembler<Aarch64Relocation>,
    src_offset: i32,
    dst_offset: i32,
    byte_size: usize,
    four_state: bool,
    state_pages: StatePageBases,
) {
    let last_chunk = byte_size.div_ceil(8).saturating_sub(1);
    let last_len = byte_size.saturating_sub(last_chunk * 8);
    let plane_count = if four_state { 2 } else { 1 };
    dynasm!(ops ; .arch aarch64 ; fmov d4, x17);
    let copy_done = if last_len != 8 {
        let full_chunk = ops.new_dynamic_label();
        let copy_done = ops.new_dynamic_label();
        emit_load_imm(ops, SCRATCH0, (last_chunk * 8) as u64);
        dynasm!(ops ; .arch aarch64 ; cmp x17, x16 ; b.ne =>full_chunk);
        for plane in 0..plane_count {
            let delta = (plane * byte_size) as i32;
            emit_sparse_chunk_copy(
                ops,
                src_offset + delta,
                dst_offset + delta,
                last_len,
                state_pages,
            );
        }
        dynasm!(ops ; .arch aarch64 ; b =>copy_done ; =>full_chunk);
        Some(copy_done)
    } else {
        None
    };
    for plane in 0..plane_count {
        let delta = (plane * byte_size) as i32;
        emit_sparse_chunk_copy(ops, src_offset + delta, dst_offset + delta, 8, state_pages);
    }
    if let Some(copy_done) = copy_done {
        dynasm!(ops ; .arch aarch64 ; =>copy_done);
    }
}

pub(super) fn emit_sparse_chunk_copy(
    ops: &mut VecAssembler<Aarch64Relocation>,
    src_offset: i32,
    dst_offset: i32,
    byte_len: usize,
    state_pages: StatePageBases,
) {
    dynasm!(ops ; .arch aarch64 ; fmov x17, d4);
    emit_sparse_indexed_address(ops, SCRATCH0, src_offset, state_pages);
    emit_sparse_indexed_address(ops, 30, dst_offset, state_pages);
    if byte_len == 8 {
        dynasm!(ops ; .arch aarch64 ; ldr d3, [x16] ; str d3, [x30]);
        return;
    }
    if byte_len >= 4 {
        dynasm!(ops ; .arch aarch64 ; ldr w17, [x16], #4 ; str w17, [x30], #4);
    }
    if byte_len % 4 >= 2 {
        dynasm!(ops ; .arch aarch64 ; ldrh w17, [x16], #2 ; strh w17, [x30], #2);
    }
    if byte_len % 2 == 1 {
        dynasm!(ops ; .arch aarch64 ; ldrb w17, [x16] ; strb w17, [x30]);
    }
}

/// Load and clear a work bitmap without clobbering its loaded value while
/// materializing the store address. x17 holds the result; x16 is scratch.
pub(super) fn emit_sparse_bitmap_take(
    ops: &mut VecAssembler<Aarch64Relocation>,
    offset: i64,
    state_pages: StatePageBases,
) {
    let (base, relative) = select_memory_base(
        BaseReg::SimState,
        offset,
        SCRATCH1,
        OpSize::S64,
        false,
        state_pages,
    );
    if let Some(load) = memory_access_encoding(SCRATCH1, base, relative, OpSize::S64, false)
        && let Some(clear) = memory_access_encoding(31, base, relative, OpSize::S64, true)
    {
        ops.push_u32(load);
        ops.push_u32(clear);
    } else {
        let (base, relative) = select_vector_memory_base(BaseReg::SimState, offset, state_pages);
        emit_address(ops, base, relative);
        dynasm!(ops ; .arch aarch64 ; ldr x17, [x16] ; str xzr, [x16]);
    }
}

/// Form state + offset + x17, preserving both x17 and the other scratch
/// address. The general address helper may use x17 for a large immediate.
pub(super) fn emit_sparse_indexed_address(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    offset: i32,
    state_pages: StatePageBases,
) {
    debug_assert!(matches!(destination, 16 | 30));
    let (base, relative) =
        select_vector_memory_base(BaseReg::SimState, i64::from(offset), state_pages);
    if relative == 0 {
        dynasm!(ops ; .arch aarch64 ; add X(destination), X(base), x17);
        return;
    }
    if !emit_add_sub_immediate(ops, destination, base, relative) {
        if let Some((high, low)) = add_sub_immediate_pair(relative) {
            let _ = emit_add_sub_immediate(ops, destination, base, high);
            let _ = emit_add_sub_immediate(ops, destination, destination, low);
        } else {
            emit_load_imm(ops, destination, relative as u64);
            dynasm!(ops ; .arch aarch64 ; add X(destination), X(base), X(destination));
        }
    }
    dynasm!(ops ; .arch aarch64 ; add X(destination), X(destination), x17);
}

pub(super) fn emit_mem_fill(
    ops: &mut VecAssembler<Aarch64Relocation>,
    dst_offset: i32,
    byte_len: usize,
    value: u8,
    state_pages: StatePageBases,
) {
    if byte_len == 0 {
        return;
    }
    let pattern = u64::from(value) * 0x0101_0101_0101_0101;
    let (base, offset) =
        select_vector_memory_base(BaseReg::SimState, i64::from(dst_offset), state_pages);
    emit_address_to(ops, SCRATCH0, base, offset);
    emit_load_imm(ops, 30, pattern);
    let vector_chunks = byte_len / 16;
    if vector_chunks != 0 {
        dynasm!(ops
            ; .arch aarch64
            ; fmov d0, x30
            ; dup v0.d2, v0.d[0]
        );
        if vector_chunks <= 32 {
            for _ in 0..vector_chunks {
                dynasm!(ops ; .arch aarch64 ; str q0, [x16], #16);
            }
        } else {
            let loop_label = ops.new_dynamic_label();
            let done = ops.new_dynamic_label();
            emit_load_imm(ops, SCRATCH1, vector_chunks as u64);
            dynasm!(ops
                ; .arch aarch64
                ; =>loop_label
                ; cbz x17, =>done
                ; str q0, [x16], #16
                ; sub x17, x17, #1
                ; b =>loop_label
                ; =>done
            );
        }
    }
    let remainder = byte_len % 16;
    if remainder >= 8 {
        dynasm!(ops ; .arch aarch64 ; str x30, [x16], #8);
    }
    if remainder % 8 >= 4 {
        dynasm!(ops ; .arch aarch64 ; str w30, [x16], #4);
    }
    if remainder % 4 >= 2 {
        dynasm!(ops ; .arch aarch64 ; strh w30, [x16], #2);
    }
    if remainder % 2 == 1 {
        dynasm!(ops ; .arch aarch64 ; strb w30, [x16]);
    }
}
