//! MIR instruction dispatch and machine instruction encoding.

use super::*;

pub(super) fn emit_inst(
    asm: &mut CodeAssembler,
    inst: &MInst,
    assignment: &AssignmentMap,
    func: &MFunction,
    constant_table_labels: &[CodeLabel],
    spill_register_cache: SpillRegisterCache,
    mut continuation_label: Option<&mut CodeLabel>,
) -> Result<bool, IcedError> {
    let mut bound_continuation = false;
    match inst {
        MInst::X86Simd(X86SimdInst::Scratch128 { .. }) => {}
        MInst::X86Simd(X86SimdInst::Zero128 { dst }) => {
            match assignment
                .x86_vector(*dst)
                .expect("verified x86 vector assignment")
            {
                X86VectorLocation::Register(register) => {
                    let destination = x86_vec_to_xmm(register);
                    if func.target_features.avx() {
                        asm.vpxor(destination, destination, destination)?;
                    } else {
                        asm.pxor(destination, destination)?;
                    }
                }
                X86VectorLocation::Stack(stack_offset) => {
                    let destination = xmmword_ptr(mem_operand(BaseReg::StackFrame, stack_offset));
                    if func.target_features.avx() {
                        asm.vpxor(xmm5, xmm5, xmm5)?;
                        asm.vmovdqu(destination, xmm5)?;
                    } else {
                        asm.pxor(xmm5, xmm5)?;
                        asm.movdqu(destination, xmm5)?;
                    }
                }
            }
        }
        MInst::X86Simd(X86SimdInst::Pack128 {
            dst,
            low,
            high,
            scratch,
        }) => {
            let low = preg_to_reg64(resolve(assignment, *low));
            let high = preg_to_reg64(resolve(assignment, *high));
            match assignment
                .x86_vector(*dst)
                .expect("verified x86 vector assignment")
            {
                X86VectorLocation::Register(register) => {
                    let destination = x86_vec_to_xmm(register);
                    asm.movq(destination, low)?;
                    if low == high {
                        asm.punpcklqdq(destination, destination)?;
                    } else {
                        let scratch = scratch.expect("verified pack scratch vector");
                        match assignment
                            .x86_vector(scratch)
                            .expect("verified x86 vector scratch assignment")
                        {
                            X86VectorLocation::Register(register) => {
                                let scratch = x86_vec_to_xmm(register);
                                debug_assert_ne!(destination, scratch);
                                asm.movq(scratch, high)?;
                                asm.punpcklqdq(destination, scratch)?;
                            }
                            X86VectorLocation::Stack(stack_offset) => {
                                asm.mov(
                                    qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)),
                                    low,
                                )?;
                                asm.mov(
                                    qword_ptr(mem_operand(
                                        BaseReg::StackFrame,
                                        stack_offset
                                            .checked_add(8)
                                            .expect("verified vector spill offset fits i32"),
                                    )),
                                    high,
                                )?;
                                let source =
                                    xmmword_ptr(mem_operand(BaseReg::StackFrame, stack_offset));
                                // Pack128 otherwise uses legacy scalar/SSE
                                // instructions. Keep this rare spill reload
                                // baseline as well, so it cannot introduce an
                                // untracked AVX requirement into the image.
                                asm.movdqu(destination, source)?;
                            }
                        }
                    }
                }
                X86VectorLocation::Stack(stack_offset) => {
                    asm.mov(
                        qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)),
                        low,
                    )?;
                    asm.mov(
                        qword_ptr(mem_operand(
                            BaseReg::StackFrame,
                            stack_offset
                                .checked_add(8)
                                .expect("verified vector spill offset fits i32"),
                        )),
                        high,
                    )?;
                }
            }
        }
        MInst::X86Simd(X86SimdInst::Load128 { dst, base, offset }) => {
            let source = xmmword_ptr(mem_operand(*base, *offset));
            match assignment
                .x86_vector(*dst)
                .expect("verified x86 vector assignment")
            {
                X86VectorLocation::Register(register) => {
                    let destination = x86_vec_to_xmm(register);
                    if func.target_features.avx() {
                        asm.vmovdqu(destination, source)?;
                    } else {
                        asm.movdqu(destination, source)?;
                    }
                }
                X86VectorLocation::Stack(stack_offset) => {
                    if func.target_features.avx() {
                        asm.vmovdqu(xmm5, source)?;
                        asm.vmovdqu(
                            xmmword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)),
                            xmm5,
                        )?;
                    } else {
                        asm.movdqu(xmm5, source)?;
                        asm.movdqu(
                            xmmword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)),
                            xmm5,
                        )?;
                    }
                }
            }
        }
        MInst::X86Simd(X86SimdInst::Binary128 { op, dst, lhs, rhs }) => {
            let destination = assignment
                .x86_vector(*dst)
                .expect("verified x86 vector destination assignment");
            let lhs = assignment
                .x86_vector(*lhs)
                .expect("verified x86 vector lhs assignment");
            let rhs = assignment
                .x86_vector(*rhs)
                .expect("verified x86 vector rhs assignment");
            let output = match destination {
                X86VectorLocation::Register(register) => x86_vec_to_xmm(register),
                X86VectorLocation::Stack(_) => xmm5,
            };
            match (lhs, rhs) {
                (X86VectorLocation::Register(lhs), X86VectorLocation::Register(rhs)) => {
                    let lhs = x86_vec_to_xmm(lhs);
                    let rhs = x86_vec_to_xmm(rhs);
                    if func.target_features.avx() {
                        match op {
                            X86SimdBinaryOp::And => asm.vpand(output, lhs, rhs)?,
                            X86SimdBinaryOp::Or => asm.vpor(output, lhs, rhs)?,
                            X86SimdBinaryOp::Xor => asm.vpxor(output, lhs, rhs)?,
                        }
                    } else {
                        if output != lhs {
                            asm.movdqa(output, lhs)?;
                        }
                        match op {
                            X86SimdBinaryOp::And => asm.pand(output, rhs)?,
                            X86SimdBinaryOp::Or => asm.por(output, rhs)?,
                            X86SimdBinaryOp::Xor => asm.pxor(output, rhs)?,
                        }
                    }
                }
                (X86VectorLocation::Register(lhs), X86VectorLocation::Stack(rhs)) => {
                    let lhs = x86_vec_to_xmm(lhs);
                    let rhs = xmmword_ptr(mem_operand(BaseReg::StackFrame, rhs));
                    if func.target_features.avx() {
                        match op {
                            X86SimdBinaryOp::And => asm.vpand(output, lhs, rhs)?,
                            X86SimdBinaryOp::Or => asm.vpor(output, lhs, rhs)?,
                            X86SimdBinaryOp::Xor => asm.vpxor(output, lhs, rhs)?,
                        }
                    } else {
                        if output != lhs {
                            asm.movdqa(output, lhs)?;
                        }
                        match op {
                            X86SimdBinaryOp::And => asm.pand(output, rhs)?,
                            X86SimdBinaryOp::Or => asm.por(output, rhs)?,
                            X86SimdBinaryOp::Xor => asm.pxor(output, rhs)?,
                        }
                    }
                }
                (X86VectorLocation::Stack(lhs), X86VectorLocation::Register(rhs)) => {
                    let lhs = xmmword_ptr(mem_operand(BaseReg::StackFrame, lhs));
                    let rhs = x86_vec_to_xmm(rhs);
                    if func.target_features.avx() {
                        asm.vmovdqu(output, lhs)?;
                        match op {
                            X86SimdBinaryOp::And => asm.vpand(output, output, rhs)?,
                            X86SimdBinaryOp::Or => asm.vpor(output, output, rhs)?,
                            X86SimdBinaryOp::Xor => asm.vpxor(output, output, rhs)?,
                        }
                    } else {
                        asm.movdqu(output, lhs)?;
                        match op {
                            X86SimdBinaryOp::And => asm.pand(output, rhs)?,
                            X86SimdBinaryOp::Or => asm.por(output, rhs)?,
                            X86SimdBinaryOp::Xor => asm.pxor(output, rhs)?,
                        }
                    }
                }
                (X86VectorLocation::Stack(lhs), X86VectorLocation::Stack(rhs)) => {
                    let lhs = xmmword_ptr(mem_operand(BaseReg::StackFrame, lhs));
                    let rhs = xmmword_ptr(mem_operand(BaseReg::StackFrame, rhs));
                    if func.target_features.avx() {
                        asm.vmovdqu(output, lhs)?;
                        match op {
                            X86SimdBinaryOp::And => asm.vpand(output, output, rhs)?,
                            X86SimdBinaryOp::Or => asm.vpor(output, output, rhs)?,
                            X86SimdBinaryOp::Xor => asm.vpxor(output, output, rhs)?,
                        }
                    } else {
                        asm.movdqu(output, lhs)?;
                        match op {
                            X86SimdBinaryOp::And => asm.pand(output, rhs)?,
                            X86SimdBinaryOp::Or => asm.por(output, rhs)?,
                            X86SimdBinaryOp::Xor => asm.pxor(output, rhs)?,
                        }
                    }
                }
            }
            if let X86VectorLocation::Stack(stack_offset) = destination {
                let destination = xmmword_ptr(mem_operand(BaseReg::StackFrame, stack_offset));
                if func.target_features.avx() {
                    asm.vmovdqu(destination, output)?;
                } else {
                    asm.movdqu(destination, output)?;
                }
            }
        }
        MInst::X86Simd(X86SimdInst::Store128 { base, offset, src }) => {
            let destination = xmmword_ptr(mem_operand(*base, *offset));
            match assignment
                .x86_vector(*src)
                .expect("verified x86 vector assignment")
            {
                X86VectorLocation::Register(register) => {
                    let source = x86_vec_to_xmm(register);
                    if func.target_features.avx() {
                        asm.vmovdqu(destination, source)?;
                    } else {
                        asm.movdqu(destination, source)?;
                    }
                }
                X86VectorLocation::Stack(stack_offset) => {
                    let source = xmmword_ptr(mem_operand(BaseReg::StackFrame, stack_offset));
                    if func.target_features.avx() {
                        asm.vmovdqu(xmm5, source)?;
                        asm.vmovdqu(destination, xmm5)?;
                    } else {
                        asm.movdqu(xmm5, source)?;
                        asm.movdqu(destination, xmm5)?;
                    }
                }
            }
        }
        MInst::Mov { dst, src } => {
            let d_preg = resolve(assignment, *dst);
            let s_preg = resolve(assignment, *src);
            if d_preg != s_preg {
                asm.mov(preg_to_reg64(d_preg), preg_to_reg64(s_preg))?;
            }
        }
        MInst::Mov32 { dst, src } => {
            let d = preg_to_reg32(resolve(assignment, *dst));
            let s = preg_to_reg32(resolve(assignment, *src));
            // `mov r32, r32` is required even for an assigned self-copy: it
            // is the zero-extension specified by Mov32.
            asm.mov(d, s)?;
        }

        MInst::LoadImm { dst, value } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            if *value == 0 {
                // xor eax, eax is shorter than mov rax, 0
                let d32 = preg_to_reg32(resolve(assignment, *dst));
                asm.xor(d32, d32)?;
            } else if *value <= u32::MAX as u64 {
                // mov r32, imm32 (zero-extends to 64-bit)
                let d32 = preg_to_reg32(resolve(assignment, *dst));
                asm.mov(d32, *value as u32)?;
            } else {
                asm.mov(d, *value as i64)?;
            }
        }

        // The assigned register is reserved for the following pseudo use; its
        // incoming bits are irrelevant and require no machine instruction.
        MInst::Scratch { .. } => {}

        MInst::LoadConstantTableAddr { dst, table } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            // MIR verification guarantees that the table identity exists.
            asm.lea(d, ptr(constant_table_labels[table.0]))?;
        }

        MInst::Load {
            dst,
            base,
            offset,
            size,
        } => {
            let d_preg = resolve(assignment, *dst);
            if let (BaseReg::StackFrame, OpSize::S64, Some(register)) =
                (*base, *size, spill_register_cache.register(*offset))
            {
                asm.movq(preg_to_reg64(d_preg), register)?;
                return Ok(false);
            }
            let mem = mem_operand(*base, *offset);
            match size {
                OpSize::S8 => {
                    let d32 = preg_to_reg32(d_preg);
                    asm.movzx(d32, byte_ptr(mem))?;
                }
                OpSize::S16 => {
                    let d32 = preg_to_reg32(d_preg);
                    asm.movzx(d32, word_ptr(mem))?;
                }
                OpSize::S32 => {
                    let d32 = preg_to_reg32(d_preg);
                    asm.mov(d32, dword_ptr(mem))?;
                }
                OpSize::S64 => {
                    let d64 = preg_to_reg64(d_preg);
                    asm.mov(d64, qword_ptr(mem))?;
                }
            }
        }

        MInst::Store {
            base,
            offset,
            src,
            size,
        } => {
            let s_preg = resolve(assignment, *src);
            if let (BaseReg::StackFrame, OpSize::S64, Some(register)) =
                (*base, *size, spill_register_cache.register(*offset))
            {
                asm.movq(register, preg_to_reg64(s_preg))?;
                return Ok(false);
            }
            let mem = mem_operand(*base, *offset);
            match size {
                OpSize::S8 => {
                    asm.mov(byte_ptr(mem), preg_to_reg8(s_preg))?;
                }
                OpSize::S16 => {
                    asm.mov(word_ptr(mem), preg_to_reg16(s_preg))?;
                }
                OpSize::S32 => {
                    asm.mov(dword_ptr(mem), preg_to_reg32(s_preg))?;
                }
                OpSize::S64 => {
                    asm.mov(qword_ptr(mem), preg_to_reg64(s_preg))?;
                }
            }
        }

        MInst::AndStoreImm {
            base,
            offset,
            size,
            imm,
        } => {
            let mem = mem_operand(*base, *offset);
            match size {
                OpSize::S8 => asm.and(byte_ptr(mem), *imm as i32)?,
                OpSize::S16 => asm.and(word_ptr(mem), *imm as i32)?,
                OpSize::S32 => asm.and(dword_ptr(mem), *imm as i32)?,
                OpSize::S64 => asm.and(qword_ptr(mem), *imm as i32)?,
            }
        }
        MInst::OrStoreImm {
            base,
            offset,
            size,
            imm,
        } => {
            let mem = mem_operand(*base, *offset);
            match size {
                OpSize::S8 => asm.or(byte_ptr(mem), *imm as i32)?,
                OpSize::S16 => asm.or(word_ptr(mem), *imm as i32)?,
                OpSize::S32 => asm.or(dword_ptr(mem), *imm as i32)?,
                OpSize::S64 => asm.or(qword_ptr(mem), *imm as i32)?,
            }
        }

        MInst::MemCopy {
            src_offset,
            dst_offset,
            byte_len,
        } => {
            if *byte_len == 0 {
                return Ok(false);
            }
            if src_offset == dst_offset {
                return Ok(false);
            }
            let src_end = i64::from(*src_offset) + *byte_len as i64;
            let dst_end = i64::from(*dst_offset) + *byte_len as i64;
            let nonoverlapping =
                src_end <= i64::from(*dst_offset) || dst_end <= i64::from(*src_offset);
            // Exact offsets let short copies use segment-relative operands
            // directly. For overlap, choose memmove's safe direction. This
            // avoids materializing the GS/FS base and borrowing RSI/RDI/RCX
            // for what is commonly only one scalar or vector transfer.
            if nonoverlapping || *byte_len <= 256 {
                emit_direct_memcopy(
                    asm,
                    *src_offset,
                    *dst_offset,
                    *byte_len,
                    !nonoverlapping && dst_offset > src_offset,
                )?;
                return Ok(false);
            }
            let qwords = byte_len / 8;
            let rem = byte_len % 8;
            if rem != 0 {
                asm.mov(qword_ptr(scratch_operand(0)), rax)?;
            }
            if qwords != 0 {
                asm.mov(qword_ptr(scratch_operand(1)), rcx)?;
            }
            asm.mov(qword_ptr(scratch_operand(2)), rsi)?;
            asm.mov(qword_ptr(scratch_operand(3)), rdi)?;
            emit_state_base(asm, rsi)?;
            asm.mov(rdi, rsi)?;
            if *src_offset != 0 {
                asm.add(rsi, *src_offset)?;
            }
            if *dst_offset != 0 {
                asm.add(rdi, *dst_offset)?;
            }
            if qwords > 0 {
                asm.mov(rcx, qwords as i64)?;
                // MOVS has the same forward-copy semantics as the scalar loop
                // it replaces, while current x86-64 implementations execute
                // REP MOVS as a dedicated bulk-copy path.  It also avoids one
                // generated branch and six scalar instructions per qword.
                asm.rep().movsq()?;
            }
            if rem >= 4 {
                asm.mov(eax, dword_ptr(rsi))?;
                asm.mov(dword_ptr(rdi), eax)?;
                asm.add(rsi, 4)?;
                asm.add(rdi, 4)?;
            }
            if rem % 4 >= 2 {
                asm.mov(ax, word_ptr(rsi))?;
                asm.mov(word_ptr(rdi), ax)?;
                asm.add(rsi, 2)?;
                asm.add(rdi, 2)?;
            }
            if rem % 2 == 1 {
                asm.mov(al, byte_ptr(rsi))?;
                asm.mov(byte_ptr(rdi), al)?;
            }
            asm.mov(rdi, qword_ptr(scratch_operand(3)))?;
            asm.mov(rsi, qword_ptr(scratch_operand(2)))?;
            if qwords != 0 {
                asm.mov(rcx, qword_ptr(scratch_operand(1)))?;
            }
            if rem != 0 {
                asm.mov(rax, qword_ptr(scratch_operand(0)))?;
            }
        }

        MInst::MemFill {
            dst_offset,
            byte_len,
            value,
        } => {
            if *byte_len == 0 {
                return Ok(false);
            }
            let qwords = byte_len / 8;
            let rem = byte_len % 8;
            let pattern = u64::from(*value) * 0x0101_0101_0101_0101;

            asm.mov(qword_ptr(scratch_operand(0)), rax)?;
            if qwords != 0 {
                asm.mov(qword_ptr(scratch_operand(1)), rcx)?;
            }
            asm.mov(qword_ptr(scratch_operand(2)), rdi)?;
            emit_state_base(asm, rdi)?;
            if *dst_offset != 0 {
                asm.add(rdi, *dst_offset)?;
            }
            asm.mov(rax, pattern as i64)?;
            if qwords != 0 {
                asm.mov(rcx, qwords as i64)?;
                asm.rep().stosq()?;
            }
            if rem >= 4 {
                asm.mov(dword_ptr(rdi), eax)?;
                asm.add(rdi, 4)?;
            }
            if rem % 4 >= 2 {
                asm.mov(word_ptr(rdi), ax)?;
                asm.add(rdi, 2)?;
            }
            if rem % 2 == 1 {
                asm.mov(byte_ptr(rdi), al)?;
            }
            asm.mov(rdi, qword_ptr(scratch_operand(2)))?;
            if qwords != 0 {
                asm.mov(rcx, qword_ptr(scratch_operand(1)))?;
            }
            asm.mov(rax, qword_ptr(scratch_operand(0)))?;
        }

        MInst::SparseCommit {
            src_offset,
            dst_offset,
            byte_size,
            dirty_words_offset,
            dirty_word_count,
            summary_words_offset,
            summary_word_count,
            four_state,
        } => {
            // The fixed scratch set is an explicit MIR clobber.  Allocation
            // keeps live-through values in other registers or gives them a
            // home, so the generated loop needs no hidden save/restore pair.
            let chunk_count = byte_size.div_ceil(8);
            let last_chunk = chunk_count.saturating_sub(1);
            let last_len = byte_size.saturating_sub(last_chunk * 8);
            let plane_count = if *four_state { 2 } else { 1 };

            for summary_index in 0..*summary_word_count {
                let summary_offset = *summary_words_offset + (summary_index * 8) as i32;
                asm.mov(
                    rax,
                    qword_ptr(mem_operand(BaseReg::SimState, summary_offset)),
                )?;
                asm.mov(
                    qword_ptr(mem_operand(BaseReg::SimState, summary_offset)),
                    0i32,
                )?;
                let mut summary_loop = asm.create_label();
                let mut summary_next = asm.create_label();
                let final_summary = summary_index + 1 == *summary_word_count;
                let use_continuation = final_summary && continuation_label.is_some();
                let mut local_summary_done = asm.create_label();
                let summary_done = if use_continuation {
                    continuation_label
                        .as_deref()
                        .copied()
                        .expect("checked sparse continuation label")
                } else {
                    local_summary_done
                };
                asm.set_label(&mut summary_loop)?;
                asm.test(rax, rax)?;
                asm.je(summary_done)?;
                asm.bsf(rcx, rax)?;
                asm.btr(rax, rcx)?;
                asm.mov(rdx, rcx)?;
                if summary_index != 0 {
                    asm.add(rdx, (summary_index * 64) as i32)?;
                }
                asm.cmp(rdx, *dirty_word_count as i32)?;
                asm.jae(summary_next)?;

                asm.mov(rdi, rdx)?;
                asm.shl(rdi, 3)?;
                asm.mov(
                    r8,
                    qword_ptr(mem_operand_indexed(
                        BaseReg::SimState,
                        *dirty_words_offset,
                        rdi,
                        1,
                    )),
                )?;
                asm.mov(
                    qword_ptr(mem_operand_indexed(
                        BaseReg::SimState,
                        *dirty_words_offset,
                        rdi,
                        1,
                    )),
                    0i32,
                )?;

                let mut dirty_loop = asm.create_label();
                let mut dirty_next = asm.create_label();
                asm.set_label(&mut dirty_loop)?;
                asm.test(r8, r8)?;
                asm.je(summary_next)?;
                asm.bsf(r9, r8)?;
                asm.btr(r8, r9)?;
                asm.mov(rdi, rdx)?;
                asm.shl(rdi, 6)?;
                asm.add(rdi, r9)?;
                asm.cmp(rdi, chunk_count as i32)?;
                asm.jae(dirty_next)?;
                asm.shl(rdi, 3)?;

                if last_len == 8 {
                    for plane in 0..plane_count {
                        let delta = (plane * *byte_size) as i32;
                        emit_sparse_chunk_copy(
                            asm,
                            *src_offset + delta,
                            *dst_offset + delta,
                            rdi,
                            8,
                        )?;
                    }
                } else {
                    let mut full = asm.create_label();
                    asm.cmp(rdi, (last_chunk * 8) as i32)?;
                    asm.jne(full)?;
                    for plane in 0..plane_count {
                        let delta = (plane * *byte_size) as i32;
                        emit_sparse_chunk_copy(
                            asm,
                            *src_offset + delta,
                            *dst_offset + delta,
                            rdi,
                            last_len,
                        )?;
                    }
                    asm.jmp(dirty_next)?;
                    asm.set_label(&mut full)?;
                    for plane in 0..plane_count {
                        let delta = (plane * *byte_size) as i32;
                        emit_sparse_chunk_copy(
                            asm,
                            *src_offset + delta,
                            *dst_offset + delta,
                            rdi,
                            8,
                        )?;
                    }
                }
                asm.set_label(&mut dirty_next)?;
                asm.jmp(dirty_loop)?;
                asm.set_label(&mut summary_next)?;
                asm.jmp(summary_loop)?;
                if use_continuation {
                    asm.set_label(
                        continuation_label
                            .as_deref_mut()
                            .expect("checked sparse continuation label"),
                    )?;
                    bound_continuation = true;
                } else {
                    asm.set_label(&mut local_summary_done)?;
                }
            }
        }

        MInst::SparseMarkActive {
            active_index,
            active_bits_offset,
            ..
        } => {
            let word_offset = i32::try_from((*active_index as usize / 64) * 8)
                .expect("verified sparse active bitmap offset fits i32");
            let bit = *active_index % 64;
            asm.bts(
                qword_ptr(mem_operand(
                    BaseReg::SimState,
                    active_bits_offset
                        .checked_add(word_offset)
                        .expect("verified sparse active bitmap offset fits i32"),
                )),
                bit,
            )?;
        }

        MInst::SparseCommitWorklist {
            descriptor_table,
            active_bits_offset,
            active_capacity,
        } => {
            emit_sparse_inline_commits(
                asm,
                func.constant_table(*descriptor_table)
                    .expect("verified sparse descriptor table"),
                *active_bits_offset,
            )?;
            bound_continuation = emit_sparse_commit_worklist(
                asm,
                constant_table_labels[descriptor_table.0],
                *active_bits_offset,
                *active_capacity,
                continuation_label,
            )?;
        }

        MInst::LoadPtr {
            dst,
            ptr,
            offset,
            size,
        } => {
            let d_preg = resolve(assignment, *dst);
            let ptr = preg_to_reg64(resolve(assignment, *ptr));
            let mem = mem_operand_ptr(ptr, *offset);
            match size {
                OpSize::S8 => {
                    asm.movzx(preg_to_reg32(d_preg), byte_ptr(mem))?;
                }
                OpSize::S16 => {
                    asm.movzx(preg_to_reg32(d_preg), word_ptr(mem))?;
                }
                OpSize::S32 => {
                    asm.mov(preg_to_reg32(d_preg), dword_ptr(mem))?;
                }
                OpSize::S64 => {
                    asm.mov(preg_to_reg64(d_preg), qword_ptr(mem))?;
                }
            }
        }

        MInst::StorePtr {
            ptr,
            offset,
            src,
            size,
        }
        | MInst::ReleaseStorePtr {
            ptr,
            offset,
            src,
            size,
        } => {
            let ptr = preg_to_reg64(resolve(assignment, *ptr));
            let s_preg = resolve(assignment, *src);
            let mem = mem_operand_ptr(ptr, *offset);
            // x86-64 TSO gives plain aligned stores release-store ordering:
            // earlier payload stores cannot become visible after this publish store.
            match size {
                OpSize::S8 => {
                    asm.mov(byte_ptr(mem), preg_to_reg8(s_preg))?;
                }
                OpSize::S16 => {
                    asm.mov(word_ptr(mem), preg_to_reg16(s_preg))?;
                }
                OpSize::S32 => {
                    asm.mov(dword_ptr(mem), preg_to_reg32(s_preg))?;
                }
                OpSize::S64 => {
                    asm.mov(qword_ptr(mem), preg_to_reg64(s_preg))?;
                }
            }
        }

        MInst::LoadIndexed {
            dst,
            base,
            offset,
            index,
            scale,
            size,
            ..
        } => {
            let d_preg = resolve(assignment, *dst);
            let idx = preg_to_reg64(resolve(assignment, *index));
            let mem = mem_operand_indexed(*base, *offset, idx, *scale);
            match size {
                OpSize::S8 => {
                    asm.movzx(preg_to_reg32(d_preg), byte_ptr(mem))?;
                }
                OpSize::S16 => {
                    asm.movzx(preg_to_reg32(d_preg), word_ptr(mem))?;
                }
                OpSize::S32 => {
                    asm.mov(preg_to_reg32(d_preg), dword_ptr(mem))?;
                }
                OpSize::S64 => {
                    asm.mov(preg_to_reg64(d_preg), qword_ptr(mem))?;
                }
            }
        }

        MInst::PackedLaneCompare {
            dst,
            rhs,
            kind,
            offset,
            lane_count,
            element_stride,
            bit_offset,
            field_width,
            ..
        } => {
            let d64 = preg_to_reg64(resolve(assignment, *dst));
            let d32 = preg_to_reg32(resolve(assignment, *dst));
            let stride = usize::from(*element_stride);
            debug_assert!(matches!(stride, 1 | 2 | 4));
            debug_assert_eq!(usize::from(*lane_count) * stride % 16, 0);

            // XMM registers are outside the GPR allocator. The pseudo is one
            // indivisible emission unit, so these temporaries cannot overlap
            // another generated operation.
            if let PackedLaneCompareRhs::Scalar(value) = rhs {
                let scalar = preg_to_reg32(resolve(assignment, *value));
                asm.movd(xmm1, scalar)?;
                match stride {
                    1 => {
                        asm.punpcklbw(xmm1, xmm1)?;
                        asm.punpcklwd(xmm1, xmm1)?;
                        asm.pshufd(xmm1, xmm1, 0)?;
                    }
                    2 => {
                        asm.pshuflw(xmm1, xmm1, 0)?;
                        asm.pshufd(xmm1, xmm1, 0)?;
                    }
                    4 => asm.pshufd(xmm1, xmm1, 0)?,
                    _ => unreachable!(),
                }
                asm.movdqa(xmm3, xmm1)?;
            }
            let storage_width = stride * 8;
            let needs_mask = usize::from(*field_width) != storage_width;
            debug_assert!(
                matches!(kind, CmpKind::Eq | CmpKind::Ne) || !needs_mask,
                "ordered packed comparisons require a full physical lane"
            );
            if needs_mask {
                let mask = if *field_width == 32 {
                    u32::MAX
                } else {
                    (1u32 << *field_width) - 1
                };
                asm.mov(d32, mask)?;
                asm.movd(xmm4, d32)?;
                match stride {
                    1 => {
                        asm.punpcklbw(xmm4, xmm4)?;
                        asm.punpcklwd(xmm4, xmm4)?;
                        asm.pshufd(xmm4, xmm4, 0)?;
                    }
                    2 => {
                        asm.pshuflw(xmm4, xmm4, 0)?;
                        asm.pshufd(xmm4, xmm4, 0)?;
                    }
                    4 => asm.pshufd(xmm4, xmm4, 0)?,
                    _ => unreachable!(),
                }
            }
            let ordered = !matches!(kind, CmpKind::Eq | CmpKind::Ne);
            let unsigned = matches!(
                kind,
                CmpKind::LtU | CmpKind::LeU | CmpKind::GtU | CmpKind::GeU
            );
            let invert = matches!(
                kind,
                CmpKind::Ne | CmpKind::LeU | CmpKind::LeS | CmpKind::GeU | CmpKind::GeS
            );
            let swap = matches!(
                kind,
                CmpKind::LtU | CmpKind::LtS | CmpKind::GeU | CmpKind::GeS
            );
            if ordered && unsigned {
                asm.mov(d32, 1u32 << (storage_width - 1))?;
                asm.movd(xmm5, d32)?;
                match stride {
                    1 => {
                        asm.punpcklbw(xmm5, xmm5)?;
                        asm.punpcklwd(xmm5, xmm5)?;
                        asm.pshufd(xmm5, xmm5, 0)?;
                    }
                    2 => {
                        asm.pshuflw(xmm5, xmm5, 0)?;
                        asm.pshufd(xmm5, xmm5, 0)?;
                    }
                    4 => asm.pshufd(xmm5, xmm5, 0)?,
                    _ => unreachable!(),
                }
            }
            asm.pxor(xmm2, xmm2)?;
            let lanes_per_chunk = 16 / stride;
            for lane_base in (0..usize::from(*lane_count)).step_by(lanes_per_chunk) {
                let chunk_offset = offset
                    .checked_add((lane_base * stride) as i32)
                    .expect("packed lane compare offset must fit i32");
                asm.movdqu(
                    xmm0,
                    xmmword_ptr(mem_operand(BaseReg::SimState, chunk_offset)),
                )?;
                if let PackedLaneCompareRhs::Memory { offset, .. } = rhs {
                    let rhs_chunk_offset = offset
                        .checked_add((lane_base * stride) as i32)
                        .expect("packed lane compare RHS offset must fit i32");
                    asm.movdqu(
                        xmm1,
                        xmmword_ptr(mem_operand(BaseReg::SimState, rhs_chunk_offset)),
                    )?;
                } else {
                    asm.movdqa(xmm1, xmm3)?;
                }
                if *bit_offset != 0 {
                    match stride {
                        2 => {
                            asm.psrlw(xmm0, u32::from(*bit_offset))?;
                            if matches!(rhs, PackedLaneCompareRhs::Memory { .. }) {
                                asm.psrlw(xmm1, u32::from(*bit_offset))?;
                            }
                        }
                        4 => {
                            asm.psrld(xmm0, u32::from(*bit_offset))?;
                            if matches!(rhs, PackedLaneCompareRhs::Memory { .. }) {
                                asm.psrld(xmm1, u32::from(*bit_offset))?;
                            }
                        }
                        _ => unreachable!("byte-lane shifts are rejected by ISel"),
                    }
                }
                if needs_mask {
                    asm.pand(xmm0, xmm4)?;
                    asm.pand(xmm1, xmm4)?;
                }
                let result = if ordered {
                    if unsigned {
                        asm.pxor(xmm0, xmm5)?;
                        asm.pxor(xmm1, xmm5)?;
                    }
                    match (stride, swap) {
                        (1, false) => asm.pcmpgtb(xmm0, xmm1)?,
                        (1, true) => asm.pcmpgtb(xmm1, xmm0)?,
                        (2, false) => asm.pcmpgtw(xmm0, xmm1)?,
                        (2, true) => asm.pcmpgtw(xmm1, xmm0)?,
                        (4, false) => asm.pcmpgtd(xmm0, xmm1)?,
                        (4, true) => asm.pcmpgtd(xmm1, xmm0)?,
                        _ => unreachable!(),
                    }
                    if swap { xmm1 } else { xmm0 }
                } else {
                    match stride {
                        1 => asm.pcmpeqb(xmm0, xmm1)?,
                        2 => asm.pcmpeqw(xmm0, xmm1)?,
                        4 => asm.pcmpeqd(xmm0, xmm1)?,
                        _ => unreachable!(),
                    }
                    xmm0
                };
                if invert {
                    let inverse_temp = if ordered && swap { xmm0 } else { xmm1 };
                    asm.pcmpeqd(inverse_temp, inverse_temp)?;
                    asm.pxor(result, inverse_temp)?;
                }
                match stride {
                    1 => asm.pmovmskb(d32, result)?,
                    2 => {
                        asm.packsswb(result, result)?;
                        asm.pmovmskb(d32, result)?;
                        asm.and(d32, 0xff)?;
                    }
                    4 => asm.movmskps(d32, result)?,
                    _ => unreachable!(),
                }
                if lane_base != 0 {
                    asm.shl(d64, lane_base as u32)?;
                }
                asm.movd(result, d32)?;
                asm.por(xmm2, result)?;
            }
            asm.movq(d64, xmm2)?;
        }

        MInst::PackedByteAffineCompare {
            dst,
            base,
            rhs,
            kind,
        } => {
            let d64 = preg_to_reg64(resolve(assignment, *dst));
            let d32 = preg_to_reg32(resolve(assignment, *dst));
            let base32 = preg_to_reg32(resolve(assignment, *base));
            let rhs32 = preg_to_reg32(resolve(assignment, *rhs));

            // Read both inputs before using the output register as scratch:
            // allocation may coalesce a dying input with this definition.
            asm.movd(xmm3, base32)?;
            asm.punpcklbw(xmm3, xmm3)?;
            asm.punpcklwd(xmm3, xmm3)?;
            asm.pshufd(xmm3, xmm3, 0)?;
            asm.movd(xmm4, rhs32)?;
            asm.punpcklbw(xmm4, xmm4)?;
            asm.punpcklwd(xmm4, xmm4)?;
            asm.pshufd(xmm4, xmm4, 0)?;

            asm.mov(d64, 0x0706_0504_0302_0100_i64)?;
            asm.movq(xmm0, d64)?;
            asm.mov(d64, 0x0f0e_0d0c_0b0a_0908_i64)?;
            asm.movq(xmm2, d64)?;
            asm.punpcklqdq(xmm0, xmm2)?;
            asm.paddb(xmm0, xmm3)?;

            let invert = match kind {
                CmpKind::Eq => {
                    asm.pcmpeqb(xmm0, xmm4)?;
                    false
                }
                CmpKind::Ne => {
                    asm.pcmpeqb(xmm0, xmm4)?;
                    true
                }
                CmpKind::LtU | CmpKind::GeU => {
                    // Saturated rhs-lhs is nonzero exactly when lhs < rhs.
                    asm.movdqa(xmm2, xmm4)?;
                    asm.psubusb(xmm2, xmm0)?;
                    asm.pxor(xmm1, xmm1)?;
                    asm.pcmpeqb(xmm2, xmm1)?;
                    asm.movdqa(xmm0, xmm2)?;
                    matches!(kind, CmpKind::LtU)
                }
                CmpKind::GtU | CmpKind::LeU => {
                    // Saturated lhs-rhs is nonzero exactly when lhs > rhs.
                    asm.movdqa(xmm2, xmm0)?;
                    asm.psubusb(xmm2, xmm4)?;
                    asm.pxor(xmm1, xmm1)?;
                    asm.pcmpeqb(xmm2, xmm1)?;
                    asm.movdqa(xmm0, xmm2)?;
                    matches!(kind, CmpKind::GtU)
                }
                CmpKind::LtS | CmpKind::GeS => {
                    asm.movdqa(xmm2, xmm4)?;
                    asm.pcmpgtb(xmm2, xmm0)?;
                    asm.movdqa(xmm0, xmm2)?;
                    matches!(kind, CmpKind::GeS)
                }
                CmpKind::GtS | CmpKind::LeS => {
                    asm.pcmpgtb(xmm0, xmm4)?;
                    matches!(kind, CmpKind::LeS)
                }
            };
            asm.pmovmskb(d32, xmm0)?;
            if invert {
                asm.xor(d32, 0xffff)?;
            }
        }

        MInst::LoadPtrIndexed {
            dst,
            ptr,
            offset,
            index,
            size,
        } => {
            let d_preg = resolve(assignment, *dst);
            let ptr = preg_to_reg64(resolve(assignment, *ptr));
            let idx = preg_to_reg64(resolve(assignment, *index));
            let mem = mem_operand_ptr_indexed(ptr, *offset, idx);
            match size {
                OpSize::S8 => {
                    asm.movzx(preg_to_reg32(d_preg), byte_ptr(mem))?;
                }
                OpSize::S16 => {
                    asm.movzx(preg_to_reg32(d_preg), word_ptr(mem))?;
                }
                OpSize::S32 => {
                    asm.mov(preg_to_reg32(d_preg), dword_ptr(mem))?;
                }
                OpSize::S64 => {
                    asm.mov(preg_to_reg64(d_preg), qword_ptr(mem))?;
                }
            }
        }

        MInst::StorePtrIndexed {
            ptr,
            offset,
            index,
            src,
            size,
        }
        | MInst::ReleaseStorePtrIndexed {
            ptr,
            offset,
            index,
            src,
            size,
        } => {
            let ptr = preg_to_reg64(resolve(assignment, *ptr));
            let idx = preg_to_reg64(resolve(assignment, *index));
            let s_preg = resolve(assignment, *src);
            let mem = mem_operand_ptr_indexed(ptr, *offset, idx);
            // x86-64 TSO gives plain aligned stores release-store ordering:
            // earlier payload stores cannot become visible after this publish store.
            match size {
                OpSize::S8 => {
                    asm.mov(byte_ptr(mem), preg_to_reg8(s_preg))?;
                }
                OpSize::S16 => {
                    asm.mov(word_ptr(mem), preg_to_reg16(s_preg))?;
                }
                OpSize::S32 => {
                    asm.mov(dword_ptr(mem), preg_to_reg32(s_preg))?;
                }
                OpSize::S64 => {
                    asm.mov(qword_ptr(mem), preg_to_reg64(s_preg))?;
                }
            }
        }

        MInst::StoreIndexed {
            base,
            offset,
            index,
            src,
            size,
            ..
        } => {
            let s_preg = resolve(assignment, *src);
            let idx = preg_to_reg64(resolve(assignment, *index));
            let mem = mem_operand_indexed(*base, *offset, idx, 1);
            match size {
                OpSize::S8 => {
                    asm.mov(byte_ptr(mem), preg_to_reg8(s_preg))?;
                }
                OpSize::S16 => {
                    asm.mov(word_ptr(mem), preg_to_reg16(s_preg))?;
                }
                OpSize::S32 => {
                    asm.mov(dword_ptr(mem), preg_to_reg32(s_preg))?;
                }
                OpSize::S64 => {
                    asm.mov(qword_ptr(mem), preg_to_reg64(s_preg))?;
                }
            }
        }

        MInst::OrStoreIndexed {
            base,
            offset,
            index,
            src,
            size,
            ..
        } => {
            let s_preg = resolve(assignment, *src);
            let idx = preg_to_reg64(resolve(assignment, *index));
            let mem = mem_operand_indexed(*base, *offset, idx, 1);
            match size {
                OpSize::S8 => asm.or(byte_ptr(mem), preg_to_reg8(s_preg))?,
                OpSize::S16 => asm.or(word_ptr(mem), preg_to_reg16(s_preg))?,
                OpSize::S32 => asm.or(dword_ptr(mem), preg_to_reg32(s_preg))?,
                OpSize::S64 => asm.or(qword_ptr(mem), preg_to_reg64(s_preg))?,
            }
        }

        // ── ALU 3-operand → 2-operand ──
        // x86: dst = dst OP src. If dst != lhs, insert mov dst, lhs first.
        // The opcode, selected by ISel, carries the x86 word width.  Do not
        // recover it from a VReg-side dataflow fact here.
        MInst::Add { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Add, false)?;
        }
        MInst::Add32 { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Add, true)?;
        }
        MInst::Sub { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Sub, false)?;
        }
        MInst::Sub32 { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Sub, true)?;
        }
        MInst::Mul { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Mul, false)?;
        }
        MInst::Mul32 { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Mul, true)?;
        }
        MInst::MulImm { dst, src, imm } | MInst::MulImm32 { dst, src, imm } => {
            let destination = resolve(assignment, *dst);
            let source = resolve(assignment, *src);
            let word32 = matches!(inst, MInst::MulImm32 { .. });
            let source64 = preg_to_reg64(source);
            if destination == source && matches!(*imm, 2 | 4 | 8) {
                let shift = (*imm as u32).trailing_zeros();
                if word32 {
                    asm.shl(preg_to_reg32(destination), shift)?;
                } else {
                    asm.shl(preg_to_reg64(destination), shift)?;
                }
            } else {
                // Use the same scaled-address forms as small constant shifts.
                // A 32-bit destination preserves multiplication modulo 2^32,
                // even though address calculation uses the full source value.
                let address = match *imm {
                    2 => Some(ptr(source64 + source64)),
                    3 | 5 | 9 => Some(ptr(source64 + source64 * (*imm as u32 - 1))),
                    4 | 8 => Some(ptr(source64 * *imm as u32)),
                    _ => None,
                };
                if let Some(address) = address {
                    if word32 {
                        asm.lea(preg_to_reg32(destination), address)?;
                    } else {
                        asm.lea(preg_to_reg64(destination), address)?;
                    }
                } else if word32 {
                    asm.imul_3(preg_to_reg32(destination), preg_to_reg32(source), *imm)?;
                } else {
                    asm.imul_3(preg_to_reg64(destination), source64, *imm)?;
                }
            }
        }
        MInst::UMulHi { dst, lhs, rhs } => {
            // x86-64: mul r64 → RDX:RAX = RAX × r64. We want RDX (high 64).
            // Must handle aliasing: lhs/rhs may be in RAX or RDX.
            let d = preg_to_reg64(resolve(assignment, *dst));
            let l = preg_to_reg64(resolve(assignment, *lhs));
            let r = preg_to_reg64(resolve(assignment, *rhs));

            if r == rax && l != rax {
                // rhs is in RAX; mul is commutative, so mul l instead
                asm.mul(l)?;
            } else if r == rax && l == rax {
                asm.mul(rax)?;
            } else {
                // Normal case: mov rax, lhs; mul rhs
                if rax != l {
                    asm.mov(rax, l)?;
                }
                asm.mul(r)?;
            }
            if d != rdx {
                asm.mov(d, rdx)?;
            }
        }
        MInst::And { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::And, false)?;
        }
        MInst::And32 { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::And, true)?;
        }
        MInst::Or { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Or, false)?;
        }
        MInst::Or32 { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Or, true)?;
        }
        MInst::Xor { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Xor, false)?;
        }
        MInst::Xor32 { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Xor, true)?;
        }

        // Variable shifts use BMI2's arbitrary-count three-operand form when
        // selected for this function; the baseline encoding consumes CL.
        MInst::Shr { dst, lhs, rhs } => {
            emit_shift(
                asm,
                assignment,
                *dst,
                *lhs,
                *rhs,
                ShiftOp::Shr,
                func.target_features.variable_shift_encoding(),
            )?;
        }
        MInst::Shl { dst, lhs, rhs } => {
            emit_shift(
                asm,
                assignment,
                *dst,
                *lhs,
                *rhs,
                ShiftOp::Shl,
                func.target_features.variable_shift_encoding(),
            )?;
        }
        MInst::Sar { dst, lhs, rhs } => {
            emit_shift(
                asm,
                assignment,
                *dst,
                *lhs,
                *rhs,
                ShiftOp::Sar,
                func.target_features.variable_shift_encoding(),
            )?;
        }

        // Immediate ALU widths are explicit for the same reason as binary ALU.
        MInst::AndImm { dst, src, imm } => {
            emit_and_imm(
                asm,
                resolve(assignment, *dst),
                resolve(assignment, *src),
                *imm,
            )?;
        }
        MInst::AndImm32 { dst, src, imm } => {
            emit_and_imm(
                asm,
                resolve(assignment, *dst),
                resolve(assignment, *src),
                u64::from(*imm),
            )?;
        }
        MInst::OrImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            emit_or_imm64(asm, d, *imm)?;
        }
        MInst::ShrImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            asm.shr(d, *imm as u32)?;
        }
        MInst::ShlImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s && (1..=3).contains(imm) {
                // Array strides can use the scaled index without first copying
                // a source that must remain live after the shift.
                if *imm == 1 {
                    asm.lea(d, ptr(s + s))?;
                } else {
                    asm.lea(d, ptr(s * (1u32 << imm)))?;
                }
            } else {
                if d != s {
                    asm.mov(d, s)?;
                }
                asm.shl(d, *imm as u32)?;
            }
        }
        MInst::SarImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            asm.sar(d, *imm as u32)?;
        }

        MInst::AddImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                // Use LEA for non-destructive add-immediate
                asm.lea(d, qword_ptr(s + *imm))?;
            } else {
                asm.add(d, *imm)?;
            }
        }
        MInst::SubImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s
                && let Some(displacement) = imm.checked_neg()
            {
                asm.lea(d, ptr(s + displacement))?;
            } else {
                if d != s {
                    asm.mov(d, s)?;
                }
                asm.sub(d, *imm)?;
            }
        }

        MInst::Cmp {
            dst,
            lhs,
            rhs,
            kind,
        } => {
            let l = preg_to_reg64(resolve(assignment, *lhs));
            let r = preg_to_reg64(resolve(assignment, *rhs));
            asm.cmp(l, r)?;
            let d8 = preg_to_reg8(resolve(assignment, *dst));
            let d32 = preg_to_reg32(resolve(assignment, *dst));
            emit_setcc(asm, d8, *kind)?;
            asm.movzx(d32, d8)?;
        }
        MInst::CmpImm {
            dst,
            lhs,
            imm,
            kind,
        } => {
            let l = preg_to_reg64(resolve(assignment, *lhs));
            if *imm == 0 && matches!(kind, CmpKind::Eq | CmpKind::Ne) {
                // test reg, reg is shorter than cmp reg, 0
                asm.test(l, l)?;
            } else {
                asm.cmp(l, *imm)?;
            }
            let d8 = preg_to_reg8(resolve(assignment, *dst));
            let d32 = preg_to_reg32(resolve(assignment, *dst));
            emit_setcc(asm, d8, *kind)?;
            asm.movzx(d32, d8)?;
        }

        MInst::BitNot { dst, src } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            asm.not(d)?;
        }

        MInst::Neg { dst, src } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            asm.neg(d)?;
        }

        MInst::Popcnt { dst, src } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            asm.popcnt(d, s)?;
        }

        MInst::Bsf { dst, src } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            asm.bsf(d, s)?;
        }

        MInst::Bsr { dst, src } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            asm.bsr(d, s)?;
        }

        MInst::BsrOr {
            dst,
            src,
            zero_value,
        } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if let Some(done) = continuation_label {
                asm.bsr(d, s)?;
                asm.jne(*done)?;
                asm.mov(d, *zero_value as i64)?;
                asm.set_label(done)?;
                bound_continuation = true;
            } else {
                let mut done = asm.create_label();
                asm.bsr(d, s)?;
                asm.jne(done)?;
                asm.mov(d, *zero_value as i64)?;
                asm.set_label(&mut done)?;
            }
        }

        MInst::Pext { dst, src, mask } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            let m = preg_to_reg64(resolve(assignment, *mask));
            asm.pext(d, s, m)?;
        }

        MInst::Pdep { dst, src, mask } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            let m = preg_to_reg64(resolve(assignment, *mask));
            asm.pdep(d, s, m)?;
        }

        MInst::Select {
            dst,
            cond,
            true_val,
            false_val,
        } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let c = preg_to_reg64(resolve(assignment, *cond));
            let tv = preg_to_reg64(resolve(assignment, *true_val));
            let fv = preg_to_reg64(resolve(assignment, *false_val));
            asm.test(c, c)?;
            if d == tv {
                // dst already holds true_val; conditionally overwrite with false_val
                asm.cmove(d, fv)?;
            } else {
                if d != fv {
                    asm.mov(d, fv)?;
                }
                asm.cmovne(d, tv)?;
            }
        }

        MInst::CmpSelect {
            dst,
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
        } => {
            emit_cmp_select(
                asm, assignment, *dst, *lhs, *rhs, *kind, *true_val, *false_val,
            )?;
        }

        MInst::CmpImmSelect {
            dst,
            lhs,
            imm,
            kind,
            true_val,
            false_val,
        } => {
            emit_cmp_imm_select(
                asm, assignment, *dst, *lhs, *imm, *kind, *true_val, *false_val,
            )?;
        }

        MInst::GuardedCmpSelect {
            dst,
            guard,
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
        } => {
            bound_continuation = emit_guarded_cmp_select(
                asm,
                assignment,
                *dst,
                *guard,
                *lhs,
                *rhs,
                *kind,
                *true_val,
                *false_val,
                continuation_label,
            )?;
        }

        // Branch and Jump are handled in the main emit loop (with phi moves).
        MInst::Branch { .. }
        | MInst::BranchPred { .. }
        | MInst::JumpTable { .. }
        | MInst::Jump { .. } => {
            unreachable!("Branch/Jump should be handled in main emit loop");
        }

        MInst::UDiv { dst, lhs, rhs } => {
            emit_divrem(asm, assignment, *dst, *lhs, *rhs, DivOp::Div)?;
        }
        MInst::URem { dst, lhs, rhs } => {
            emit_divrem(asm, assignment, *dst, *lhs, *rhs, DivOp::Rem)?;
        }
        MInst::SDiv { dst, lhs, rhs } => {
            emit_divrem(asm, assignment, *dst, *lhs, *rhs, DivOp::SDiv)?;
        }
        MInst::SRem { dst, lhs, rhs } => {
            emit_divrem(asm, assignment, *dst, *lhs, *rhs, DivOp::SRem)?;
        }

        MInst::Return | MInst::ReturnError { .. } => {
            // Handled in the main emit loop (jumps to shared epilogue)
            unreachable!("Return/ReturnError should be handled by the main emit loop");
        }
    }
    Ok(bound_continuation)
}
