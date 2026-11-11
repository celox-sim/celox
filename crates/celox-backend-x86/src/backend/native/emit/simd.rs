//! 128-bit vector instruction encoding.

use super::*;

/// Emit one 128-bit vector instruction.
pub(super) fn emit_simd(
    asm: &mut EmitAssembler,
    inst: &X86SimdInst,
    assignment: &AssignmentMap,
    func: &MFunction,
    context: EmissionContext,
) -> Result<(), IcedError> {
    match inst {
        X86SimdInst::Scratch128 { .. } => {}
        X86SimdInst::Zero128 { dst } => {
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
                    let destination =
                        xmmword_ptr(mem_operand(&context, BaseReg::StackFrame, stack_offset));
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
        X86SimdInst::Pack128 {
            dst,
            low,
            high,
            scratch,
        } => {
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
                                    qword_ptr(mem_operand(
                                        &context,
                                        BaseReg::StackFrame,
                                        stack_offset,
                                    )),
                                    low,
                                )?;
                                asm.mov(
                                    qword_ptr(mem_operand(
                                        &context,
                                        BaseReg::StackFrame,
                                        stack_offset
                                            .checked_add(8)
                                            .expect("verified vector spill offset fits i32"),
                                    )),
                                    high,
                                )?;
                                let source = xmmword_ptr(mem_operand(
                                    &context,
                                    BaseReg::StackFrame,
                                    stack_offset,
                                ));
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
                        qword_ptr(mem_operand(&context, BaseReg::StackFrame, stack_offset)),
                        low,
                    )?;
                    asm.mov(
                        qword_ptr(mem_operand(
                            &context,
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
        X86SimdInst::Load128 { dst, base, offset } => {
            let source = xmmword_ptr(mem_operand(&context, *base, *offset));
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
                            xmmword_ptr(mem_operand(&context, BaseReg::StackFrame, stack_offset)),
                            xmm5,
                        )?;
                    } else {
                        asm.movdqu(xmm5, source)?;
                        asm.movdqu(
                            xmmword_ptr(mem_operand(&context, BaseReg::StackFrame, stack_offset)),
                            xmm5,
                        )?;
                    }
                }
            }
        }
        X86SimdInst::Binary128 { op, dst, lhs, rhs } => {
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
                    let rhs = xmmword_ptr(mem_operand(&context, BaseReg::StackFrame, rhs));
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
                    let lhs = xmmword_ptr(mem_operand(&context, BaseReg::StackFrame, lhs));
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
                    let lhs = xmmword_ptr(mem_operand(&context, BaseReg::StackFrame, lhs));
                    let rhs = xmmword_ptr(mem_operand(&context, BaseReg::StackFrame, rhs));
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
                let destination =
                    xmmword_ptr(mem_operand(&context, BaseReg::StackFrame, stack_offset));
                if func.target_features.avx() {
                    asm.vmovdqu(destination, output)?;
                } else {
                    asm.movdqu(destination, output)?;
                }
            }
        }
        X86SimdInst::Store128 { base, offset, src } => {
            let destination = xmmword_ptr(mem_operand(&context, *base, *offset));
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
                    let source =
                        xmmword_ptr(mem_operand(&context, BaseReg::StackFrame, stack_offset));
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
    }
    Ok(())
}
