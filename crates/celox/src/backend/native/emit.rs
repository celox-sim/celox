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

fn preg_to_reg16(preg: PhysReg) -> AsmRegister16 {
    match preg {
        PhysReg::RAX => ax,
        PhysReg::RCX => cx,
        PhysReg::RDX => dx,
        PhysReg::RBX => bx,
        PhysReg::RSI => si,
        PhysReg::RDI => di,
        PhysReg::R8 => r8w,
        PhysReg::R9 => r9w,
        PhysReg::R10 => r10w,
        PhysReg::R11 => r11w,
        PhysReg::R12 => r12w,
        PhysReg::R13 => r13w,
        PhysReg::R14 => r14w,
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
// Phi resolution
// ────────────────────────────────────────────────────────────────

/// Emit Mov instructions to resolve phi nodes when jumping to a target block.
/// For each phi in the target block, if the source (from this predecessor) and
/// the dst are assigned to different physical registers, emit `mov dst, src`.
fn emit_phi_moves(
    asm: &mut CodeAssembler,
    terminator: &MInst,
    pred_block_id: BlockId,
    func: &MFunction,
    assignment: &AssignmentMap,
) -> Result<(), IcedError> {
    // Collect target block IDs from the terminator
    let targets: Vec<BlockId> = match terminator {
        MInst::Jump { target } => vec![*target],
        MInst::Branch { true_bb, false_bb, .. } => vec![*true_bb, *false_bb],
        _ => return Ok(()),
    };

    for target_id in targets {
        let target_block = func.blocks.iter().find(|b| b.id == target_id);
        let Some(target_block) = target_block else { continue };
        for phi in &target_block.phis {
            for (source_pred, source_vreg) in &phi.sources {
                if *source_pred == pred_block_id {
                    let src_preg = resolve(assignment, *source_vreg);
                    let dst_preg = resolve(assignment, phi.dst);
                    if src_preg != dst_preg {
                        let src_reg = preg_to_reg64(src_preg);
                        let dst_reg = preg_to_reg64(dst_preg);
                        asm.mov(dst_reg, src_reg)?;
                    }
                }
            }
        }
    }
    Ok(())
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
    for (_bi, block) in func.blocks.iter().enumerate() {
        let label = block_labels.get_mut(&block.id).unwrap();
        asm.set_label(label)?;

        for (_inst_idx, inst) in block.insts.iter().enumerate() {
            // Before terminators that jump to blocks with phis, emit phi Movs
            if inst.is_terminator() {
                emit_phi_moves(&mut asm, inst, block.id, func, assignment)?;
            }

            match inst {
                MInst::Return => {
                    asm.xor(eax, eax)?;
                    asm.jmp(epilogue_label)?;
                }
                MInst::ReturnError { code } => {
                    asm.mov(eax, *code as u32)?;
                    asm.jmp(epilogue_label)?;
                }
                MInst::UDiv { dst, lhs, rhs } => {
                    emit_divrem(&mut asm, assignment, *dst, *lhs, *rhs, DivOp::Div)?;
                }
                MInst::URem { dst, lhs, rhs } => {
                    emit_divrem(&mut asm, assignment, *dst, *lhs, *rhs, DivOp::Rem)?;
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

        // ── ALU 3-operand → 2-operand ──
        // x86: dst = dst OP src. If dst != lhs, insert mov dst, lhs first.
        MInst::Add { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Add)?;
        }
        MInst::Sub { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Sub)?;
        }
        MInst::Mul { dst, lhs, rhs } => {
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Mul)?;
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

        // Shifts: rhs must be in CL. The emit phase moves rhs to RCX
        // rather than relying on assignment constraints, to avoid conflicts
        // when multiple shifts with different amounts coexist.
        MInst::Shr { dst, lhs, rhs } => {
            emit_shift(asm, assignment, *dst, *lhs, *rhs, ShiftOp::Shr)?;
        }
        MInst::Shl { dst, lhs, rhs } => {
            emit_shift(asm, assignment, *dst, *lhs, *rhs, ShiftOp::Shl)?;
        }
        MInst::Sar { dst, lhs, rhs } => {
            emit_shift(asm, assignment, *dst, *lhs, *rhs, ShiftOp::Sar)?;
        }

        // Immediate ALU
        MInst::AndImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            emit_and_imm64(asm, d, *imm)?;
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
        MInst::SarImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            asm.sar(d, *imm as u32)?;
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
                // ISel guarantees val is a fresh copy (dead after this BFI).
                // 1. dst = base_word (may be no-op if d == bw)
                if d != bw {
                    asm.mov(d, bw)?;
                }
                // 2. Clear the field in dst
                let clear_mask = !((*mask) << *shift);
                emit_and_imm64(asm, d, clear_mask)?;
                // 3. Prepare val (clobbers v, which is dead)
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
                debug_assert!(bw != d, "BFI: val and base_word should not alias");
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
                emit_and_imm64(asm, d, field_mask)?;
                // 4. d ^= bw → result
                asm.xor(d, bw)?;
            }
        }

        MInst::Select { dst, cond, true_val, false_val } => {
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

        MInst::UDiv { dst, lhs, rhs } => {
            emit_divrem(asm, assignment, *dst, *lhs, *rhs, DivOp::Div)?;
        }
        MInst::URem { dst, lhs, rhs } => {
            emit_divrem(asm, assignment, *dst, *lhs, *rhs, DivOp::Rem)?;
        }

        MInst::Return | MInst::ReturnError { .. } => {
            // Handled in the main emit loop (jumps to shared epilogue)
            unreachable!("Return/ReturnError should be handled by the main emit loop");
        }
    }
    Ok(())
}

/// Shift operation kind.
enum ShiftOp {
    Shr,
    Shl,
    Sar,
}

/// Emit a shift instruction, moving rhs to RCX if needed.
/// Handles all aliasing cases between dst, lhs, rhs, and RCX.
fn emit_shift(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: ShiftOp,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));

    let do_shift = |asm: &mut CodeAssembler, reg: AsmRegister64| -> Result<(), IcedError> {
        match op {
            ShiftOp::Shr => asm.shr(reg, cl),
            ShiftOp::Shl => asm.shl(reg, cl),
            ShiftOp::Sar => asm.sar(reg, cl),
        }
    };

    // ISel guarantees rhs is a fresh copy (dead after this shift).
    // Assignment places it in RCX via Fixed constraint.
    debug_assert!(r == rcx, "shift rhs must be in RCX");
    if d != l {
        asm.mov(d, l)?;
    }
    do_shift(asm, d)?;
    Ok(())
}

/// Division operation kind.
enum DivOp {
    Div, // quotient in RAX
    Rem, // remainder in RDX
}

/// Emit unsigned division/remainder: `div r64`
/// x86-64 `div r64`: RDX:RAX / operand → RAX = quotient, RDX = remainder.
///
/// The assignment phase avoids placing live-across VRegs in RAX/RDX around
/// div/rem instructions, so no save/restore is needed here.
fn emit_divrem(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: DivOp,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));

    let result_reg: AsmRegister64 = match op {
        DivOp::Div => rax,
        DivOp::Rem => rdx,
    };

    // Divisor cannot be in RAX or RDX (clobbered by div).
    let effective_rhs = if r == rax || r == rdx {
        asm.mov(rcx, r)?;
        rcx
    } else {
        r
    };

    if l != rax {
        asm.mov(rax, l)?;
    }
    asm.xor(edx, edx)?;
    asm.div(effective_rhs)?;

    if d != result_reg {
        asm.mov(d, result_reg)?;
    }

    Ok(())
}

/// Helper for 2-operand binary operations (add, sub, and, or, xor).
enum BinOp {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
}

impl BinOp {
    /// Whether the operation is commutative (a op b == b op a).
    fn is_commutative(&self) -> bool {
        matches!(self, BinOp::Add | BinOp::Mul | BinOp::And | BinOp::Or | BinOp::Xor)
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
            // Non-commutative (sub): d == rhs, d != lhs.
            // Cannot xchg (would clobber lhs which may be live).
            // Use NEG + ADD: d = -d + lhs = lhs - rhs.
            asm.neg(d)?;
            asm.add(d, l)?;
            return Ok(());
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
        BinOp::Mul => asm.imul_2(d, eff_r)?,
        BinOp::And => asm.and(d, eff_r)?,
        BinOp::Or => asm.or(d, eff_r)?,
        BinOp::Xor => asm.xor(d, eff_r)?,
    }
    Ok(())
}

/// Emit AND with a potentially 64-bit immediate.
/// Uses the most efficient encoding available.
fn emit_and_imm64(
    asm: &mut CodeAssembler,
    d: AsmRegister64,
    imm: u64,
) -> Result<(), IcedError> {
    if imm == u64::MAX {
        // AND with all-ones is a no-op
        return Ok(());
    }
    let signed = imm as i64;
    if signed >= i32::MIN as i64 && signed <= i32::MAX as i64 {
        // Fits in sign-extended imm32
        asm.and(d, signed as i32)?;
    } else if imm <= u32::MAX as u64 {
        // Fits in zero-extended 32-bit: use 32-bit AND (clears upper 32 bits)
        let d32 = match d {
            _ if d == rax => eax,
            _ if d == rcx => ecx,
            _ if d == rdx => edx,
            _ if d == rbx => ebx,
            _ if d == rsi => esi,
            _ if d == rdi => edi,
            _ if d == r8 => r8d,
            _ if d == r9 => r9d,
            _ if d == r10 => r10d,
            _ if d == r11 => r11d,
            _ if d == r12 => r12d,
            _ if d == r13 => r13d,
            _ if d == r14 => r14d,
            _ => unreachable!(),
        };
        asm.and(d32, imm as i32)?;
    } else {
        // Full 64-bit: load into scratch via push/pop trick or movabs.
        // Use RAX as scratch if d != rax, otherwise use RDX.
        // We save/restore the scratch via push/pop.
        let scratch = if d != rax { rax } else { rdx };
        asm.push(scratch)?;
        asm.mov(scratch, imm as i64)?; // movabs scratch, imm64
        asm.and(d, scratch)?;
        asm.pop(scratch)?;
    }
    Ok(())
}

// ────────────────────────────────────────────────────────────────
// Multi-EU chained emission
// ────────────────────────────────────────────────────────────────

/// Compile multiple EUs into a single JIT function.
///
/// Each EU is independently compiled (ISel + regalloc + emit) producing
/// a self-contained function. We then concatenate their machine code,
/// patching each EU's `ret` instruction to jump to the next EU's entry.
/// Only the last EU actually returns to the caller.
///
/// Each EU has its own prologue/epilogue (callee-saved register save/restore,
/// R15 setup, stack frame allocation). This is slightly redundant but correct
/// and simple — the overhead is just a few pushes/pops between EUs.
pub fn emit_chained_eus(
    units: &[crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &crate::backend::MemoryLayout,
) -> Result<Vec<u8>, IcedError> {
    use super::{isel, regalloc};

    // Compile each EU independently
    let mut eu_codes: Vec<Vec<u8>> = Vec::new();
    for eu in units {
        let mut mfunc = isel::lower_execution_unit(eu, layout);
        let ra = regalloc::run_regalloc(&mut mfunc);

        let result = emit(&mfunc, &ra.assignment, ra.spill_frame_size)?;
        eu_codes.push(result.code);
    }

    // Concatenate code segments, patching `ret` in all but the last EU.
    // Each EU's epilogue ends with `ret` (0xC3). We find the last `ret`
    // in each non-final EU and replace it with a `jmp rel32` to the next EU.
    //
    // Note: `ret` = 1 byte (0xC3), `jmp rel32` = 5 bytes. We need to
    // expand the code to make room. Simplest: pad each non-final EU with
    // 4 extra bytes (nops) before the ret, then overwrite ret+nops with jmp.
    //
    // Actually, even simpler: assemble all EUs, concatenate, then patch.
    // Since ret is the very last byte, we can replace it with a 2-byte
    // `jmp short` (0xEB, offset) if the next EU starts within 127 bytes,
    // or use a 5-byte `jmp near` by inserting padding.

    // Strategy: for non-final EUs, replace the trailing `ret` with 5-byte
    // jmp near. To do this, we replace the `ret` (1 byte) with a jmp (5 bytes),
    // which means we need 4 extra bytes. We pad with nops before concatenating.
    let mut combined: Vec<u8> = Vec::new();
    let mut eu_offsets: Vec<usize> = Vec::new();

    for (i, code) in eu_codes.iter().enumerate() {
        eu_offsets.push(combined.len());
        if i < eu_codes.len() - 1 {
            // Non-final EU: append code without the trailing ret,
            // then append 5 bytes placeholder for jmp
            assert_eq!(*code.last().unwrap(), 0xC3, "EU code should end with ret");
            combined.extend_from_slice(&code[..code.len() - 1]);
            // Placeholder for jmp rel32 (will be patched below)
            combined.extend_from_slice(&[0xE9, 0x00, 0x00, 0x00, 0x00]);
        } else {
            // Final EU: keep as-is (including ret)
            combined.extend_from_slice(code);
        }
    }

    // Patch jmp targets
    for i in 0..(eu_codes.len() - 1) {
        let jmp_offset = eu_offsets[i] + eu_codes[i].len() - 1; // position of the jmp opcode
        let next_eu_offset = eu_offsets[i + 1];
        let rel = (next_eu_offset as i64) - (jmp_offset as i64 + 5); // relative to end of jmp
        combined[jmp_offset] = 0xE9; // jmp near rel32
        combined[jmp_offset + 1..jmp_offset + 5].copy_from_slice(&(rel as i32).to_le_bytes());
    }

    Ok(combined)
}
