//! MIR instruction dispatch and machine instruction encoding.

use super::*;

pub(super) fn emit_inst(
    asm: &mut EmitAssembler,
    inst: &MInst,
    assignment: &AssignmentMap,
    func: &MFunction,
    constant_table_labels: &[CodeLabel],
    spill_register_cache: SpillRegisterCache,
    continuation_label: Option<&mut CodeLabel>,
) -> Result<bool, IcedError> {
    let context = asm.context;
    let mut bound_continuation = false;
    match inst {
        MInst::FloatBinary {
            dst,
            lhs,
            rhs,
            op,
            precision,
        } => {
            floating::emit_binary(asm, assignment, *dst, *lhs, *rhs, *op, *precision)?;
        }
        MInst::FloatUnary {
            dst,
            src,
            op,
            precision,
        } => {
            floating::emit_unary(asm, assignment, *dst, *src, *op, *precision)?;
        }
        MInst::ExternArg { index, src } => {
            extern_call::emit_extern_arg(asm, *index, preg_to_reg64(resolve(assignment, *src)))?;
        }
        MInst::CallExtern { func, arg_count } => {
            extern_call::emit_call_extern(asm, *func, *arg_count)?;
        }
        MInst::ExternResult { dst } => {
            extern_call::emit_extern_result(asm, preg_to_reg64(resolve(assignment, *dst)))?;
        }
        MInst::X86Simd(simd) => {
            emit_simd(asm, simd, assignment, func, context)?;
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
            let mem = mem_operand(&context, *base, *offset);
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
            let mem = mem_operand(&context, *base, *offset);
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
            let mem = mem_operand(&context, *base, *offset);
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
            let mem = mem_operand(&context, *base, *offset);
            match size {
                OpSize::S8 => asm.or(byte_ptr(mem), *imm as i32)?,
                OpSize::S16 => asm.or(word_ptr(mem), *imm as i32)?,
                OpSize::S32 => asm.or(dword_ptr(mem), *imm as i32)?,
                OpSize::S64 => asm.or(qword_ptr(mem), *imm as i32)?,
            }
        }
        MInst::StoreImm {
            base,
            offset,
            size,
            imm,
        } => {
            let mem = mem_operand(&context, *base, *offset);
            match size {
                OpSize::S8 => asm.mov(byte_ptr(mem), *imm as i32)?,
                OpSize::S16 => asm.mov(word_ptr(mem), *imm as i32)?,
                OpSize::S32 => asm.mov(dword_ptr(mem), *imm as i32)?,
                OpSize::S64 => asm.mov(qword_ptr(mem), *imm as i32)?,
            }
        }

        MInst::MemCopy {
            src_offset,
            dst_offset,
            byte_len,
        } => {
            emit_mem_copy(asm, context, src_offset, dst_offset, byte_len)?;
        }

        MInst::MemFill {
            dst_offset,
            byte_len,
            value,
        } => {
            emit_mem_fill(asm, context, dst_offset, byte_len, value)?;
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
            bound_continuation = emit_sparse_commit(
                asm,
                context,
                src_offset,
                dst_offset,
                byte_size,
                dirty_words_offset,
                dirty_word_count,
                summary_words_offset,
                summary_word_count,
                four_state,
                continuation_label,
            )?;
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
                    &context,
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
            let mem = mem_operand_indexed(&context, *base, *offset, idx, *scale);
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
            emit_packed_lane_compare(
                asm,
                assignment,
                context,
                dst,
                rhs,
                kind,
                offset,
                lane_count,
                element_stride,
                bit_offset,
                field_width,
            )?;
        }

        MInst::PackedByteAffineCompare {
            dst,
            base,
            rhs,
            kind,
        } => {
            emit_packed_byte_affine_compare(asm, assignment, dst, base, rhs, kind)?;
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
            let mem = mem_operand_indexed(&context, *base, *offset, idx, 1);
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
            let mem = mem_operand_indexed(&context, *base, *offset, idx, 1);
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
