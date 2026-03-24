//! x86-64 code emission: MIR + physical register assignment → machine code.
//!
//! Uses iced-x86's CodeAssembler for instruction encoding.
//! ABI: System V AMD64 — sim state base in RDI (moved to R15 in prologue).
//! Function signature: `fn(unified_mem: *mut u8) -> i64`

use std::collections::BTreeMap;

use iced_x86::code_asm::*;

use crate::backend::native::mir::*;
use crate::backend::native::regalloc::assignment::{AssignmentMap, PhysReg};

/// Reserved register for simulation state base pointer.
const SIM_BASE: AsmRegister64 = r15;

// ────────────────────────────────────────────────────────────────
// PhysReg → iced-x86 register mapping
// ────────────────────────────────────────────────────────────────

fn preg_to_reg64(preg: PhysReg) -> AsmRegister64 {
    match preg {
        PhysReg::RAX => rax,
        PhysReg::RCX => rcx,
        PhysReg::RDX => rdx,
        PhysReg::RBX => rbx,
        PhysReg::RSI => rsi,
        PhysReg::RDI => rdi,
        PhysReg::R8 => r8,
        PhysReg::R9 => r9,
        PhysReg::R10 => r10,
        PhysReg::R11 => r11,
        PhysReg::R12 => r12,
        PhysReg::R13 => r13,
        PhysReg::R14 => r14,
    }
}

fn preg_to_reg32(preg: PhysReg) -> AsmRegister32 {
    match preg {
        PhysReg::RAX => eax,
        PhysReg::RCX => ecx,
        PhysReg::RDX => edx,
        PhysReg::RBX => ebx,
        PhysReg::RSI => esi,
        PhysReg::RDI => edi,
        PhysReg::R8 => r8d,
        PhysReg::R9 => r9d,
        PhysReg::R10 => r10d,
        PhysReg::R11 => r11d,
        PhysReg::R12 => r12d,
        PhysReg::R13 => r13d,
        PhysReg::R14 => r14d,
    }
}

fn preg_to_reg8(preg: PhysReg) -> AsmRegister8 {
    match preg {
        PhysReg::RAX => al,
        PhysReg::RCX => cl,
        PhysReg::RDX => dl,
        PhysReg::RBX => bl,
        PhysReg::RSI => sil,
        PhysReg::RDI => dil,
        PhysReg::R8 => r8b,
        PhysReg::R9 => r9b,
        PhysReg::R10 => r10b,
        PhysReg::R11 => r11b,
        PhysReg::R12 => r12b,
        PhysReg::R13 => r13b,
        PhysReg::R14 => r14b,
    }
}

// ────────────────────────────────────────────────────────────────
// Helper: resolve VReg to physical register
// ────────────────────────────────────────────────────────────────

fn resolve(assignment: &AssignmentMap, vreg: VReg) -> PhysReg {
    assignment
        .get(vreg)
        .unwrap_or_else(|| panic!("VReg {vreg} has no physical register assignment"))
}

// ────────────────────────────────────────────────────────────────
// Memory operand helpers
// ────────────────────────────────────────────────────────────────

fn mem_operand(base: BaseReg, offset: i32) -> AsmMemoryOperand {
    let base_reg = match base {
        BaseReg::SimState => SIM_BASE,
        BaseReg::StackFrame => rsp,
    };
    base_reg + offset
}

fn mem_operand_indexed(base: BaseReg, offset: i32, index: AsmRegister64) -> AsmMemoryOperand {
    let base_reg = match base {
        BaseReg::SimState => SIM_BASE,
        BaseReg::StackFrame => rsp,
    };
    base_reg + index + offset
}

// ────────────────────────────────────────────────────────────────
// Callee-saved register tracking
// ────────────────────────────────────────────────────────────────

const CALLEE_SAVED: &[PhysReg] = &[
    PhysReg::RBX,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
];

fn used_callee_saved(assignment: &AssignmentMap) -> Vec<PhysReg> {
    let used: std::collections::BTreeSet<PhysReg> =
        assignment.map.values().copied().collect();
    CALLEE_SAVED
        .iter()
        .copied()
        .filter(|r| used.contains(r))
        .collect()
}

// ────────────────────────────────────────────────────────────────
// Emit result
// ────────────────────────────────────────────────────────────────

/// Result of code emission: raw machine code bytes.
pub struct EmitResult {
    pub code: Vec<u8>,
    /// Stack frame size (bytes) for spill slots, excluding callee-saved pushes.
    pub frame_size: u32,
}

/// Disassemble the emitted code to a string (NASM syntax).
pub fn disassemble(code: &[u8], base_addr: u64) -> String {
    use iced_x86::{Decoder, DecoderOptions, Formatter, NasmFormatter};
    let mut decoder = Decoder::with_ip(64, code, base_addr, DecoderOptions::NONE);
    let mut formatter = NasmFormatter::new();
    let mut output = String::new();
    let mut instruction = iced_x86::Instruction::default();
    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        let mut text = String::new();
        formatter.format(&instruction, &mut text);
        output.push_str(&format!("  {:#010x}  {}\n", instruction.ip(), text));
    }
    output
}

// ────────────────────────────────────────────────────────────────
// Main emit function
// ────────────────────────────────────────────────────────────────

/// Emit x86-64 machine code for an MFunction with physical register assignment.
pub fn emit(
    func: &MFunction,
    assignment: &AssignmentMap,
    spill_frame_size: u32,
) -> Result<EmitResult, IcedError> {
    let mut asm = CodeAssembler::new(64)?;

    // Block labels
    let mut block_labels: BTreeMap<BlockId, CodeLabel> = BTreeMap::new();
    for block in &func.blocks {
        block_labels.insert(block.id, asm.create_label());
    }

    // Determine callee-saved registers used
    let callee_saved = used_callee_saved(assignment);
    let callee_push_size = (callee_saved.len() as u32) * 8;

    // Align frame: total = spill_frame_size + callee_push_size + 8 (return addr)
    // Must be 16-byte aligned before any CALL (but we don't call anything).
    // After push rbp + callee saves, RSP must be 16-byte aligned for SSE loads.
    let total_push = callee_push_size + 8; // +8 for the return address on stack
    let frame_size = if spill_frame_size == 0 {
        if total_push % 16 != 0 { 8 } else { 0 }
    } else {
        let needed = spill_frame_size;
        // Round up to maintain 16-byte alignment
        let misalign = (total_push + needed) % 16;
        if misalign == 0 { needed } else { needed + (16 - misalign) }
    };

    // ── Prologue ──
    // Save callee-saved registers
    for &reg in &callee_saved {
        asm.push(preg_to_reg64(reg))?;
    }
    // Also save R15 (sim state base) — it's callee-saved
    asm.push(SIM_BASE)?;

    // Allocate stack frame for spill slots
    if frame_size > 0 {
        asm.sub(rsp, frame_size as i32)?;
    }

    // Move sim state base from RDI (first arg) to R15
    asm.mov(SIM_BASE, rdi)?;

    // Epilogue label (shared by all Return instructions)
    let mut epilogue_label = asm.create_label();

    // ── Blocks ──
    for block in &func.blocks {
        let label = block_labels.get_mut(&block.id).unwrap();
        asm.set_label(label)?;

        for inst in &block.insts {
            match inst {
                MInst::Return => {
                    // Return 0 (success) and jump to shared epilogue
                    asm.xor(eax, eax)?;
                    asm.jmp(epilogue_label)?;
                }
                _ => {
                    emit_inst(&mut asm, inst, assignment, &mut block_labels)?;
                }
            }
        }
    }

    // ── Epilogue ──
    asm.set_label(&mut epilogue_label)?;
    // Deallocate spill frame
    if frame_size > 0 {
        asm.add(rsp, frame_size as i32)?;
    }
    // Restore R15 (sim state base)
    asm.pop(SIM_BASE)?;
    // Restore callee-saved registers (reverse order)
    for &reg in callee_saved.iter().rev() {
        asm.pop(preg_to_reg64(reg))?;
    }
    asm.ret()?;

    // Assemble
    let code = asm.assemble(0x0)?;

    Ok(EmitResult { code, frame_size })
}

fn emit_inst(
    asm: &mut CodeAssembler,
    inst: &MInst,
    assignment: &AssignmentMap,
    block_labels: &mut BTreeMap<BlockId, CodeLabel>,
) -> Result<(), IcedError> {
    match inst {
        MInst::Mov { dst, src } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
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

        MInst::Load { dst, base, offset, size } => {
            let d_preg = resolve(assignment, *dst);
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

        MInst::Store { base, offset, src, size } => {
            let s_preg = resolve(assignment, *src);
            let mem = mem_operand(*base, *offset);
            match size {
                OpSize::S8 => {
                    asm.mov(byte_ptr(mem), preg_to_reg8(s_preg))?;
                }
                OpSize::S16 => {
                    // movzx doesn't apply to stores; use 16-bit register
                    // iced-x86 doesn't have direct AsmRegister16 helpers for all,
                    // so we'll use the 32-bit store masked. Actually, just use word store.
                    // For simplicity, store the low 16 bits via 32-bit reg.
                    // This works because we only care about the low bytes.
                    let s32 = preg_to_reg32(s_preg);
                    // Actually we need a proper 16-bit store to avoid overwriting.
                    // Use dword_ptr would write 4 bytes. Use word_ptr.
                    // iced-x86 code_asm: mov(word_ptr(mem), reg16)
                    // We need AsmRegister16. Let's convert.
                    let _ = s32; // unused
                    asm.mov(word_ptr(mem), preg_to_reg64(s_preg))?;
                }
                OpSize::S32 => {
                    asm.mov(dword_ptr(mem), preg_to_reg32(s_preg))?;
                }
                OpSize::S64 => {
                    asm.mov(qword_ptr(mem), preg_to_reg64(s_preg))?;
                }
            }
        }

        MInst::LoadIndexed { dst, base, offset, index, size } => {
            let d_preg = resolve(assignment, *dst);
            let idx = preg_to_reg64(resolve(assignment, *index));
            let mem = mem_operand_indexed(*base, *offset, idx);
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

        MInst::StoreIndexed { base, offset, index, src, size } => {
            let s_preg = resolve(assignment, *src);
            let idx = preg_to_reg64(resolve(assignment, *index));
            let mem = mem_operand_indexed(*base, *offset, idx);
            match size {
                OpSize::S8 => {
                    asm.mov(byte_ptr(mem), preg_to_reg8(s_preg))?;
                }
                OpSize::S16 => {
                    asm.mov(word_ptr(mem), preg_to_reg64(s_preg))?;
                }
                OpSize::S32 => {
                    asm.mov(dword_ptr(mem), preg_to_reg32(s_preg))?;
                }
                OpSize::S64 => {
                    asm.mov(qword_ptr(mem), preg_to_reg64(s_preg))?;
                }
            }
        }

        // ── ALU 3-operand → 2-operand ──
        // x86: dst = dst OP src. If dst != lhs, insert mov dst, lhs first.
        MInst::Add { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Add)?;
        }
        MInst::Sub { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Sub)?;
        }
        MInst::Mul { dst, lhs, rhs } => {
            // imul r64, r64 (2-operand form, result in first operand)
            let d = preg_to_reg64(resolve(assignment, *dst));
            let l = preg_to_reg64(resolve(assignment, *lhs));
            let r = preg_to_reg64(resolve(assignment, *rhs));
            if d != l {
                asm.mov(d, l)?;
            }
            asm.imul_2(d, r)?;
        }
        MInst::And { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::And)?;
        }
        MInst::Or { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Or)?;
        }
        MInst::Xor { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Xor)?;
        }

        // Shifts: rhs must be in CL (guaranteed by assignment constraint)
        MInst::Shr { dst, lhs, rhs } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let l = preg_to_reg64(resolve(assignment, *lhs));
            let _r = resolve(assignment, *rhs); // should be RCX
            if d != l {
                asm.mov(d, l)?;
            }
            asm.shr(d, cl)?;
        }
        MInst::Shl { dst, lhs, rhs } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let l = preg_to_reg64(resolve(assignment, *lhs));
            let _r = resolve(assignment, *rhs);
            if d != l {
                asm.mov(d, l)?;
            }
            asm.shl(d, cl)?;
        }
        MInst::Sar { dst, lhs, rhs } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let l = preg_to_reg64(resolve(assignment, *lhs));
            let _r = resolve(assignment, *rhs);
            if d != l {
                asm.mov(d, l)?;
            }
            asm.sar(d, cl)?;
        }

        // Immediate ALU
        MInst::AndImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            if *imm <= i32::MAX as u64 {
                asm.and(d, *imm as i32)?;
            } else {
                // Need 64-bit immediate — use mov to temp then and
                // Actually x86 AND r64, imm32 sign-extends. For large masks,
                // we need a different approach.
                // For now, load imm to a scratch and AND.
                // This is a limitation — we'd need a scratch register.
                // Workaround: if imm fits in 32-bit unsigned, use 32-bit operation.
                let d32 = preg_to_reg32(resolve(assignment, *dst));
                if *imm <= u32::MAX as u64 {
                    asm.and(d32, *imm as i32)?;
                } else {
                    // Full 64-bit: we'd need a scratch. For now, use rax if available.
                    // This is a known limitation.
                    unimplemented!("64-bit AND immediate > 32 bits requires scratch register");
                }
            }
        }
        MInst::OrImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            asm.or(d, *imm as i32)?;
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
            if d != s {
                asm.mov(d, s)?;
            }
            asm.shl(d, *imm as u32)?;
        }

        MInst::Cmp { dst, lhs, rhs, kind } => {
            let _d = resolve(assignment, *dst);
            let l = preg_to_reg64(resolve(assignment, *lhs));
            let r = preg_to_reg64(resolve(assignment, *rhs));
            asm.cmp(l, r)?;
            // setcc to low byte of dst, then movzx to clear upper bytes
            let d8 = preg_to_reg8(resolve(assignment, *dst));
            let d32 = preg_to_reg32(resolve(assignment, *dst));
            match kind {
                CmpKind::Eq => asm.sete(d8)?,
                CmpKind::Ne => asm.setne(d8)?,
                CmpKind::LtU => asm.setb(d8)?,
                CmpKind::LtS => asm.setl(d8)?,
                CmpKind::LeU => asm.setbe(d8)?,
                CmpKind::LeS => asm.setle(d8)?,
                CmpKind::GtU => asm.seta(d8)?,
                CmpKind::GtS => asm.setg(d8)?,
                CmpKind::GeU => asm.setae(d8)?,
                CmpKind::GeS => asm.setge(d8)?,
            }
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

        MInst::BitFieldInsert { dst, base_word, val, shift, mask } => {
            // dst = (base_word & ~(mask << shift)) | ((val & mask) << shift)
            let d = preg_to_reg64(resolve(assignment, *dst));
            let bw = preg_to_reg64(resolve(assignment, *base_word));
            let v = preg_to_reg64(resolve(assignment, *val));

            if v != d {
                // Common case: val is in a separate register from dst.
                // 1. dst = base_word (may be no-op if d == bw)
                if d != bw {
                    asm.mov(d, bw)?;
                }
                // 2. Clear the field in dst
                let clear_mask = !((*mask) << *shift) as i64;
                if clear_mask >= i32::MIN as i64 && clear_mask <= i32::MAX as i64 {
                    asm.and(d, clear_mask as i32)?;
                } else {
                    let d32 = preg_to_reg32(resolve(assignment, *dst));
                    asm.and(d32, clear_mask as i32)?;
                }
                // 3. Prepare and insert val (clobbers v, which is dead after this use)
                if *mask != u64::MAX {
                    asm.and(v, *mask as i32)?;
                }
                if *shift > 0 {
                    asm.shl(v, *shift as u32)?;
                }
                asm.or(d, v)?;
            } else {
                // v == d: val and dst alias (val dies here, dst is born).
                // bw must be different since val and base_word are both live uses.
                // Use XOR-based bitfield insert: result = bw ^ ((shifted_val ^ bw) & F)
                // This formula doesn't clobber bw.
                debug_assert!(bw != d, "val and base_word should not alias in BFI");
                // 1. d = (val & mask) << shift
                if *mask != u64::MAX {
                    asm.and(d, *mask as i32)?;
                }
                if *shift > 0 {
                    asm.shl(d, *shift as u32)?;
                }
                // 2. d ^= bw
                asm.xor(d, bw)?;
                // 3. d &= field_mask
                let field_mask = (*mask) << *shift;
                if field_mask <= i32::MAX as u64 {
                    asm.and(d, field_mask as i32)?;
                } else if field_mask <= u32::MAX as u64 {
                    let d32 = preg_to_reg32(resolve(assignment, *dst));
                    asm.and(d32, field_mask as i32)?;
                } else {
                    unimplemented!("64-bit field mask in BitFieldInsert XOR path");
                }
                // 4. d ^= bw → result
                asm.xor(d, bw)?;
            }
        }

        MInst::Select { dst, cond, true_val, false_val } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let c = preg_to_reg64(resolve(assignment, *cond));
            let tv = preg_to_reg64(resolve(assignment, *true_val));
            let fv = preg_to_reg64(resolve(assignment, *false_val));
            // test cond, cond
            asm.test(c, c)?;
            // mov dst, false_val
            if d != fv {
                asm.mov(d, fv)?;
            }
            // cmovne dst, true_val (if cond != 0, select true_val)
            asm.cmovne(d, tv)?;
        }

        MInst::Branch { cond, true_bb, false_bb } => {
            let c = preg_to_reg64(resolve(assignment, *cond));
            asm.test(c, c)?;
            let true_label = block_labels.get_mut(true_bb).unwrap();
            asm.jne(*true_label)?;
            let false_label = block_labels.get_mut(false_bb).unwrap();
            asm.jmp(*false_label)?;
        }

        MInst::Jump { target } => {
            let label = block_labels.get_mut(target).unwrap();
            asm.jmp(*label)?;
        }

        MInst::Return => {
            // Handled in the main emit loop (jumps to shared epilogue)
            unreachable!("Return should be handled by the main emit loop");
        }
    }
    Ok(())
}

/// Helper for 2-operand binary operations (add, sub, and, or, xor).
enum BinOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
}

impl BinOp {
    /// Whether the operation is commutative (a op b == b op a).
    fn is_commutative(&self) -> bool {
        matches!(self, BinOp::Add | BinOp::And | BinOp::Or | BinOp::Xor)
    }
}

fn emit_binop_rr(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: BinOp,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));

    // x86 2-operand: dst = dst OP src.
    // If dst == rhs && dst != lhs, we'd clobber rhs with `mov dst, lhs`.
    // For commutative ops, swap operands. For non-commutative, use xchg.
    let (eff_l, eff_r) = if d == r && d != l {
        if op.is_commutative() {
            (r, l) // swap: dst already has rhs, just OP with lhs
        } else {
            // Non-commutative (sub): need to save rhs, mov lhs to dst, then OP
            // Use xchg to swap dst(=rhs) and lhs
            asm.xchg(d, l)?;
            (d, l) // after xchg: d has original lhs, l has original rhs
        }
    } else {
        if d != l {
            asm.mov(d, l)?;
        }
        (d, r)
    };

    let _ = eff_l; // dst already contains the left operand
    match op {
        BinOp::Add => asm.add(d, eff_r)?,
        BinOp::Sub => asm.sub(d, eff_r)?,
        BinOp::And => asm.and(d, eff_r)?,
        BinOp::Or => asm.or(d, eff_r)?,
        BinOp::Xor => asm.xor(d, eff_r)?,
    }
    Ok(())
}
