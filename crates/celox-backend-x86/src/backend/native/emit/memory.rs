//! Direct memory copies and sparse commit emission.

use super::*;

fn emit_direct_memcopy_chunk(
    asm: &mut CodeAssembler,
    src_offset: i32,
    dst_offset: i32,
    bytes: usize,
) -> Result<(), IcedError> {
    let src = mem_operand(BaseReg::SimState, src_offset);
    let dst = mem_operand(BaseReg::SimState, dst_offset);
    match bytes {
        16 => {
            asm.movdqu(xmm0, xmmword_ptr(src))?;
            asm.movdqu(xmmword_ptr(dst), xmm0)?;
        }
        8 => {
            asm.mov(rax, qword_ptr(src))?;
            asm.mov(qword_ptr(dst), rax)?;
        }
        4 => {
            asm.mov(eax, dword_ptr(src))?;
            asm.mov(dword_ptr(dst), eax)?;
        }
        2 => {
            asm.mov(ax, word_ptr(src))?;
            asm.mov(word_ptr(dst), ax)?;
        }
        1 => {
            asm.mov(al, byte_ptr(src))?;
            asm.mov(byte_ptr(dst), al)?;
        }
        _ => unreachable!("invalid direct memory-copy chunk {bytes}"),
    }
    Ok(())
}

pub(super) fn emit_direct_memcopy(
    asm: &mut CodeAssembler,
    src_offset: i32,
    dst_offset: i32,
    byte_len: usize,
    backward: bool,
) -> Result<(), IcedError> {
    let scalar_bytes = byte_len % 16;
    if scalar_bytes != 0 {
        asm.mov(qword_ptr(scratch_operand(0)), rax)?;
    }

    if backward {
        let mut cursor = byte_len;
        while cursor >= 16 {
            cursor -= 16;
            emit_direct_memcopy_chunk(
                asm,
                src_offset + cursor as i32,
                dst_offset + cursor as i32,
                16,
            )?;
        }
        for bytes in [8usize, 4, 2, 1] {
            if cursor >= bytes {
                cursor -= bytes;
                emit_direct_memcopy_chunk(
                    asm,
                    src_offset + cursor as i32,
                    dst_offset + cursor as i32,
                    bytes,
                )?;
            }
        }
        debug_assert_eq!(cursor, 0);
    } else {
        let vector_bytes = byte_len / 16 * 16;
        let mut cursor = 0usize;
        while cursor < vector_bytes {
            emit_direct_memcopy_chunk(
                asm,
                src_offset + cursor as i32,
                dst_offset + cursor as i32,
                16,
            )?;
            cursor += 16;
        }
        for bytes in [8usize, 4, 2, 1] {
            if cursor + bytes <= byte_len {
                emit_direct_memcopy_chunk(
                    asm,
                    src_offset + cursor as i32,
                    dst_offset + cursor as i32,
                    bytes,
                )?;
                cursor += bytes;
            }
        }
        debug_assert_eq!(cursor, byte_len);
    }

    if scalar_bytes != 0 {
        asm.mov(rax, qword_ptr(scratch_operand(0)))?;
    }
    Ok(())
}

pub(super) fn emit_sparse_chunk_copy(
    asm: &mut CodeAssembler,
    src_offset: i32,
    dst_offset: i32,
    index: AsmRegister64,
    byte_len: usize,
) -> Result<(), IcedError> {
    let mut copied = 0usize;
    for bytes in [8usize, 4, 2, 1] {
        while copied + bytes <= byte_len {
            let src = mem_operand_indexed(BaseReg::SimState, src_offset + copied as i32, index, 1);
            let dst = mem_operand_indexed(BaseReg::SimState, dst_offset + copied as i32, index, 1);
            match bytes {
                8 => {
                    asm.mov(rsi, qword_ptr(src))?;
                    asm.mov(qword_ptr(dst), rsi)?;
                }
                4 => {
                    asm.mov(esi, dword_ptr(src))?;
                    asm.mov(dword_ptr(dst), esi)?;
                }
                2 => {
                    asm.mov(si, word_ptr(src))?;
                    asm.mov(word_ptr(dst), si)?;
                }
                1 => {
                    asm.mov(sil, byte_ptr(src))?;
                    asm.mov(byte_ptr(dst), sil)?;
                }
                _ => unreachable!(),
            }
            copied += bytes;
        }
    }
    Ok(())
}

fn emit_sparse_runtime_plane_copy(asm: &mut CodeAssembler) -> Result<(), IcedError> {
    let mut tail = asm.create_label();
    let mut below_four = asm.create_label();
    let mut below_two = asm.create_label();
    let mut done = asm.create_label();

    asm.cmp(r11, 8)?;
    asm.jb(tail)?;
    asm.mov(rbp, qword_ptr(rsi))?;
    asm.mov(qword_ptr(rdi), rbp)?;
    asm.jmp(done)?;

    asm.set_label(&mut tail)?;
    asm.test(r11, 4)?;
    asm.je(below_four)?;
    asm.mov(ebp, dword_ptr(rsi))?;
    asm.mov(dword_ptr(rdi), ebp)?;
    asm.add(rsi, 4)?;
    asm.add(rdi, 4)?;

    asm.set_label(&mut below_four)?;
    asm.test(r11, 2)?;
    asm.je(below_two)?;
    asm.mov(bp, word_ptr(rsi))?;
    asm.mov(word_ptr(rdi), bp)?;
    asm.add(rsi, 2)?;
    asm.add(rdi, 2)?;

    asm.set_label(&mut below_two)?;
    asm.test(r11, 1)?;
    asm.je(done)?;
    asm.mov(bpl, byte_ptr(rsi))?;
    asm.mov(byte_ptr(rdi), bpl)?;
    asm.set_label(&mut done)?;
    asm.nop()?;
    Ok(())
}

/// Commit small regions with fixed addresses and copy widths. Keep large
/// regions in the runtime worklist so their bitmap scans do not inflate code
/// size. R13 holds the captured active bits and RDI is the chunk index; all
/// scratch registers belong to SparseCommitWorklist's existing clobber set.
pub(super) fn emit_sparse_inline_commits(
    asm: &mut CodeAssembler,
    descriptors: &[u64],
    active_bits_offset: i32,
) -> Result<(), IcedError> {
    let Ok(active_start) = u64::try_from(active_bits_offset) else {
        return Ok(());
    };
    let rows = descriptors
        .as_chunks::<{ SparseCommitDescriptor::WORDS }>()
        .0;
    let is_small = |row: &[u64; SparseCommitDescriptor::WORDS]| {
        (1..=512).contains(&row[2])
            && row[4] == 1
            && row[6] == 1
            && [row[0], row[1]].into_iter().all(|offset| {
                offset
                    .checked_add(row[2] * if row[7] != 0 { 2 } else { 1 })
                    .is_some_and(|end| end <= i32::MAX as u64)
            })
            && [row[3], row[5]].into_iter().all(|offset| {
                offset
                    .checked_add(8)
                    .is_some_and(|end| end <= i32::MAX as u64)
            })
    };
    // Bound the extra text independently of the size of the descriptor table.
    let inline_count = rows.iter().filter(|row| is_small(row)).count();
    if inline_count == 0 || inline_count > 256 {
        return Ok(());
    }
    // Pulling small commits ahead of large ones is safe only when their data
    // and metadata do not overlap. MemoryLayout normally guarantees this;
    // retain the ordered runtime loop for hand-built or imported MIR as well.
    let mut ranges = vec![(
        active_start,
        active_start + rows.len().div_ceil(64) as u64 * 8,
    )];
    for row in rows {
        let Some(bytes) = row[2].checked_mul(if row[7] != 0 { 2 } else { 1 }) else {
            return Ok(());
        };
        let Some(dirty_bytes) = row[4].checked_mul(8) else {
            return Ok(());
        };
        let Some(summary_bytes) = row[6].checked_mul(8) else {
            return Ok(());
        };
        for (start, size) in [
            (row[0], bytes),
            (row[1], bytes),
            (row[3], dirty_bytes),
            (row[5], summary_bytes),
        ] {
            if size == 0 {
                continue;
            }
            let Some(end) = start.checked_add(size) else {
                return Ok(());
            };
            ranges.push((start, end));
        }
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Ok(());
    }
    for (word_index, word) in rows.chunks(64).enumerate() {
        let small = word
            .iter()
            .enumerate()
            .filter(|(_, row)| is_small(row))
            .collect::<Vec<_>>();
        if small.is_empty() {
            continue;
        }
        let mask = small.iter().fold(0u64, |mask, (bit, _)| mask | (1 << bit));
        let offset = active_bits_offset + (word_index * 8) as i32;
        let mut word_done = asm.create_label();
        asm.mov(r13, qword_ptr(mem_operand(BaseReg::SimState, offset)))?;
        asm.mov(rax, mask)?;
        asm.and(r13, rax)?;
        asm.je(word_done)?;
        asm.not(rax)?;
        asm.and(qword_ptr(mem_operand(BaseReg::SimState, offset)), rax)?;
        for (position, &(bit, row)) in small.iter().enumerate() {
            let last = position + 1 == small.len();
            let mut next = if last { word_done } else { asm.create_label() };
            asm.bt(r13, bit as u32)?;
            asm.jae(next)?;
            emit_sparse_fixed_commit(asm, row, next)?;
            if !last {
                asm.set_label(&mut next)?;
            }
        }
        asm.set_label(&mut word_done)?;
        // The next word (or the generic loop) starts with emitted instructions.
        // No extra label is bound here, preserving fallthrough label sharing.
    }
    Ok(())
}

/// A fixed descriptor whose dirty chunks fit in one word. Bound and mask the
/// dirty bitmap once, then copy using immediate addresses and plane widths.
fn emit_sparse_fixed_commit(
    asm: &mut CodeAssembler,
    row: &[u64],
    next: CodeLabel,
) -> Result<(), IcedError> {
    let bytes = row[2] as usize;
    let planes = if row[7] != 0 { 2 } else { 1 };
    let summary = qword_ptr(mem_operand(BaseReg::SimState, row[5] as i32));
    let dirty = qword_ptr(mem_operand(BaseReg::SimState, row[3] as i32));
    if bytes <= 8 {
        // The generic single-chunk path visits any nonempty dirty word, even
        // if restored metadata has a missing summary bit or dirty padding.
        asm.mov(summary, 0i32)?;
        asm.mov(rax, dirty)?;
        asm.mov(dirty, 0i32)?;
        asm.test(rax, rax)?;
        asm.je(next)?;
        asm.xor(edi, edi)?;
        for plane in 0..planes {
            let delta = (plane * bytes) as i32;
            emit_sparse_chunk_copy(
                asm,
                row[0] as i32 + delta,
                row[1] as i32 + delta,
                rdi,
                bytes,
            )?;
        }
        return Ok(());
    }
    asm.mov(rax, summary)?;
    asm.mov(summary, 0i32)?;
    asm.test(al, 1u32)?;
    asm.je(next)?;
    asm.mov(r8, dirty)?;
    asm.mov(dirty, 0i32)?;
    let chunks = bytes.div_ceil(8);
    if chunks < 64 {
        let mask = (1u64 << chunks) - 1;
        if mask <= i32::MAX as u64 {
            asm.and(r8, mask as i32)?;
        } else {
            asm.mov(rax, mask)?;
            asm.and(r8, rax)?;
        }
    }
    let mut dirty_loop = asm.create_label();
    asm.set_label(&mut dirty_loop)?;
    asm.test(r8, r8)?;
    asm.je(next)?;
    asm.bsf(rdi, r8)?;
    asm.btr(r8, rdi)?;
    asm.shl(rdi, 3)?;
    if !bytes.is_multiple_of(8) {
        let mut full_chunk = asm.create_label();
        asm.cmp(rdi, ((chunks - 1) * 8) as i32)?;
        asm.jne(full_chunk)?;
        for plane in 0..planes {
            let delta = (plane * bytes) as i32;
            emit_sparse_chunk_copy(
                asm,
                row[0] as i32 + delta,
                row[1] as i32 + delta,
                rdi,
                bytes % 8,
            )?;
        }
        asm.jmp(dirty_loop)?;
        asm.set_label(&mut full_chunk)?;
    }
    for plane in 0..planes {
        let delta = (plane * bytes) as i32;
        emit_sparse_chunk_copy(asm, row[0] as i32 + delta, row[1] as i32 + delta, rdi, 8)?;
    }
    asm.jmp(dirty_loop)?;
    Ok(())
}

pub(super) fn emit_sparse_commit_worklist(
    asm: &mut CodeAssembler,
    descriptor_label: CodeLabel,
    active_bits_offset: i32,
    active_capacity: usize,
    continuation_label: Option<&mut CodeLabel>,
) -> Result<bool, IcedError> {
    if active_capacity == 0 {
        return Ok(false);
    }

    // SparseCommitWorklist's complete scratch set is an explicit MIR
    // clobber.  Register allocation therefore protects only values that are
    // actually live through this point; the emitter must not blanket-save
    // registers here.  Callee-saved scratch registers are preserved once by
    // the function prologue/epilogue.

    // r12 = active bitmap word index, r13 = captured bits in that word.
    // Clear each word before processing it. Sparse writes cannot execute
    // concurrently with this event-tail commit, so no mark can be lost.
    // This inline region owns every allocatable GPR, so R15 can cache the
    // otherwise implicit GS base while constructing ordinary pointers.
    emit_state_base(asm, r15)?;
    asm.xor(r12d, r12d)?;

    let active_word_count = active_capacity.div_ceil(64);
    let mut active_word_loop = asm.create_label();
    let mut active_bits = asm.create_label();
    let mut active_word_next = asm.create_label();
    let mut active_next = asm.create_label();
    let mut local_active_done = asm.create_label();
    let active_done = continuation_label
        .as_deref()
        .copied()
        .unwrap_or(local_active_done);
    asm.set_label(&mut active_word_loop)?;
    asm.cmp(r12, active_word_count as i32)?;
    asm.jae(active_done)?;

    asm.mov(r13, qword_ptr(r15 + r12 * 8 + active_bits_offset))?;
    asm.mov(qword_ptr(r15 + r12 * 8 + active_bits_offset), 0i32)?;

    asm.set_label(&mut active_bits)?;
    asm.test(r13, r13)?;
    asm.je(active_word_next)?;
    asm.bsf(rcx, r13)?;
    asm.btr(r13, rcx)?;
    asm.mov(rax, r12)?;
    asm.shl(rax, 6)?;
    asm.add(rax, rcx)?;
    // Ignore padding bits in the bitmap's final word. They can only be set by
    // malformed checkpoint state, but must not index beyond the table.
    asm.cmp(rax, active_capacity as i32)?;
    asm.jae(active_bits)?;

    // Descriptor rows contain eight u64 fields and are ordered by active id.
    asm.shl(rax, 6)?;
    asm.lea(rbx, ptr(descriptor_label))?;
    asm.add(rbx, rax)?;

    // A sparse value of at most one native chunk has exactly one dirty word
    // and one summary bit. Active-bitmap membership already tells us which
    // descriptor to visit, so scanning both bitmap levels is pure overhead.
    let mut generic_summary = asm.create_label();
    asm.cmp(qword_ptr(rbx + 16), 8i32)?;
    asm.ja(generic_summary)?;
    asm.cmp(qword_ptr(rbx + 32), 1i32)?;
    asm.jne(generic_summary)?;

    // Clear the fixed summary word and take the fixed dirty word. A zero dirty
    // word is tolerated for restored/corrupt checkpoint metadata.
    asm.mov(r10, qword_ptr(rbx + 40))?;
    asm.mov(qword_ptr(r15 + r10), 0i32)?;
    asm.mov(r10, qword_ptr(rbx + 24))?;
    asm.mov(r8, qword_ptr(r15 + r10))?;
    asm.mov(qword_ptr(r15 + r10), 0i32)?;
    asm.test(r8, r8)?;
    asm.je(active_next)?;

    asm.mov(rsi, qword_ptr(rbx))?;
    asm.add(rsi, r15)?;
    asm.mov(rdi, qword_ptr(rbx + 8))?;
    asm.add(rdi, r15)?;
    asm.mov(r11, qword_ptr(rbx + 16))?;
    emit_sparse_runtime_plane_copy(asm)?;

    let mut single_plane_done = asm.create_label();
    asm.cmp(qword_ptr(rbx + 56), 0i32)?;
    asm.je(single_plane_done)?;
    asm.mov(rsi, qword_ptr(rbx))?;
    asm.add(rsi, qword_ptr(rbx + 16))?;
    asm.add(rsi, r15)?;
    asm.mov(rdi, qword_ptr(rbx + 8))?;
    asm.add(rdi, qword_ptr(rbx + 16))?;
    asm.add(rdi, r15)?;
    emit_sparse_runtime_plane_copy(asm)?;
    asm.set_label(&mut single_plane_done)?;
    asm.jmp(active_next)?;

    asm.set_label(&mut generic_summary)?;

    // r14 = summary word index.
    asm.xor(r14d, r14d)?;
    let mut summary_loop = asm.create_label();
    let mut summary_bits = asm.create_label();
    let mut summary_next = asm.create_label();
    asm.set_label(&mut summary_loop)?;
    asm.cmp(r14, qword_ptr(rbx + 48))?;
    asm.jae(active_next)?;

    // r10 = absolute state offset of this summary word; rax = its bits.
    asm.mov(r10, r14)?;
    asm.shl(r10, 3)?;
    asm.add(r10, qword_ptr(rbx + 40))?;
    asm.mov(rax, qword_ptr(r15 + r10))?;
    asm.mov(qword_ptr(r15 + r10), 0i32)?;

    asm.set_label(&mut summary_bits)?;
    asm.test(rax, rax)?;
    asm.je(summary_next)?;
    asm.bsf(rcx, rax)?;
    asm.btr(rax, rcx)?;

    // r10 = dirty-word index, then the corresponding metadata address.
    asm.mov(r10, r14)?;
    asm.shl(r10, 6)?;
    asm.add(r10, rcx)?;
    asm.cmp(r10, qword_ptr(rbx + 32))?;
    asm.jae(summary_bits)?;
    asm.mov(r9, r10)?;
    asm.shl(r9, 3)?;
    asm.add(r9, qword_ptr(rbx + 24))?;
    asm.mov(r8, qword_ptr(r15 + r9))?;
    asm.mov(qword_ptr(r15 + r9), 0i32)?;

    let mut dirty_loop = asm.create_label();
    asm.set_label(&mut dirty_loop)?;
    asm.test(r8, r8)?;
    asm.je(summary_bits)?;
    asm.bsf(rcx, r8)?;
    asm.btr(r8, rcx)?;
    // rdx = byte offset of the dirty data chunk.
    asm.mov(rdx, r10)?;
    asm.shl(rdx, 6)?;
    asm.add(rdx, rcx)?;
    asm.mov(r9, qword_ptr(rbx + 16))?;
    asm.add(r9, 7)?;
    asm.shr(r9, 3)?;
    asm.cmp(rdx, r9)?;
    asm.jae(dirty_loop)?;
    asm.shl(rdx, 3)?;

    asm.mov(rsi, qword_ptr(rbx))?;
    asm.add(rsi, rdx)?;
    asm.add(rsi, r15)?;
    asm.mov(rdi, qword_ptr(rbx + 8))?;
    asm.add(rdi, rdx)?;
    asm.add(rdi, r15)?;
    asm.mov(r11, qword_ptr(rbx + 16))?;
    asm.sub(r11, rdx)?;
    emit_sparse_runtime_plane_copy(asm)?;

    let mut plane_done = asm.create_label();
    asm.cmp(qword_ptr(rbx + 56), 0i32)?;
    asm.je(plane_done)?;
    // The second four-state plane starts byte_size bytes after the first.
    // Reconstruct the pointers because a partial first-plane copy advances
    // them by the copied 4/2-byte pieces.
    asm.mov(rsi, qword_ptr(rbx))?;
    asm.add(rsi, qword_ptr(rbx + 16))?;
    asm.add(rsi, rdx)?;
    asm.add(rsi, r15)?;
    asm.mov(rdi, qword_ptr(rbx + 8))?;
    asm.add(rdi, qword_ptr(rbx + 16))?;
    asm.add(rdi, rdx)?;
    asm.add(rdi, r15)?;
    emit_sparse_runtime_plane_copy(asm)?;
    asm.set_label(&mut plane_done)?;
    asm.jmp(dirty_loop)?;

    asm.set_label(&mut summary_next)?;
    asm.inc(r14)?;
    asm.jmp(summary_loop)?;

    asm.set_label(&mut active_next)?;
    asm.jmp(active_bits)?;
    asm.set_label(&mut active_word_next)?;
    asm.inc(r12)?;
    asm.jmp(active_word_loop)?;
    if let Some(done) = continuation_label {
        asm.set_label(done)?;
        Ok(true)
    } else {
        asm.set_label(&mut local_active_done)?;
        Ok(false)
    }
}
