//! x86-64 code emission: MIR + physical register assignment → machine code.
//!
//! Uses iced-x86's CodeAssembler for instruction encoding.
//! ABI: System V AMD64 — sim state base in RDI (moved to R15 in prologue).
//! Function signature: `fn(unified_mem: *mut u8) -> i64`

use std::collections::HashMap;

use iced_x86::code_asm::*;

use crate::backend::native::mir::*;
use crate::backend::native::regalloc::assignment::{AssignmentMap, PhysReg, PhysRegSet};

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
    let mut used = PhysRegSet::new();
    for &preg in assignment.map.values() {
        used.insert(preg);
    }
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
    /// Offsets of `jmp rel32` placeholders that need patching (naked mode only).
    /// Each entry: (offset_of_jmp_opcode, is_error_return).
    pub return_patches: Vec<(usize, bool)>,
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
/// Emit phi moves for a single target block.
fn emit_phi_moves_for_target(
    asm: &mut CodeAssembler,
    pred_block_id: BlockId,
    target_id: BlockId,
    func: &MFunction,
    assignment: &AssignmentMap,
) -> Result<(), IcedError> {
    let target_block = func.blocks.iter().find(|b| b.id == target_id);
    let Some(target_block) = target_block else { return Ok(()) };

    // Collect parallel copies: (dst_preg, src_preg)
    let mut copies: Vec<(PhysReg, PhysReg)> = Vec::new();
    for phi in &target_block.phis {
        for (source_pred, source_vreg) in &phi.sources {
            if *source_pred == pred_block_id {
                let src_preg = resolve(assignment, *source_vreg);
                let dst_preg = resolve(assignment, phi.dst);
                if src_preg != dst_preg {
                    copies.push((dst_preg, src_preg));
                }
            }
        }
    }

    if copies.is_empty() { return Ok(()); }

    // Sequentialize parallel copies, handling cycles with xchg.
    let mut done = vec![false; copies.len()];
    let mut progress = true;
    while progress {
        progress = false;
        for i in 0..copies.len() {
            if done[i] { continue; }
            let (dst, _src) = copies[i];
            let dst_is_src = copies.iter().enumerate().any(|(j, (_, s))| {
                j != i && !done[j] && *s == dst
            });
            if !dst_is_src {
                let (d, s) = copies[i];
                asm.mov(preg_to_reg64(d), preg_to_reg64(s))?;
                done[i] = true;
                progress = true;
            }
        }
    }

    // Remaining undone copies form cycles; break with xchg
    for i in 0..copies.len() {
        if done[i] { continue; }
        let (d, s) = copies[i];
        asm.xchg(preg_to_reg64(d), preg_to_reg64(s))?;
        done[i] = true;
        for j in (i + 1)..copies.len() {
            if done[j] { continue; }
            if copies[j].1 == d {
                copies[j].1 = s;
            } else if copies[j].1 == s {
                copies[j].1 = d;
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
    emit_inner(func, assignment, spill_frame_size, false)
}

/// Emit a function body without prologue/epilogue ("naked" mode).
/// Return instructions emit `ret` (0xC3) directly.
/// Used by shared-prologue EU chaining.
pub fn emit_naked(
    func: &MFunction,
    assignment: &AssignmentMap,
    spill_frame_size: u32,
) -> Result<EmitResult, IcedError> {
    emit_inner(func, assignment, spill_frame_size, true)
}

fn emit_inner(
    func: &MFunction,
    assignment: &AssignmentMap,
    spill_frame_size: u32,
    naked: bool,
) -> Result<EmitResult, IcedError> {
    let mut asm = CodeAssembler::new(64)?;
    let mut return_patches: Vec<(usize, bool)> = Vec::new();
    // Labels used as jmp targets for naked return patching
    let mut naked_return_labels: Vec<(CodeLabel, bool)> = Vec::new();

    // Block labels
    let mut block_labels: HashMap<BlockId, CodeLabel> = HashMap::new();
    for block in &func.blocks {
        block_labels.insert(block.id, asm.create_label());
    }

    let callee_saved = used_callee_saved(assignment);
    let callee_push_size = (callee_saved.len() as u32) * 8;
    let total_push = callee_push_size + 8;
    let frame_size = {
        let misalign = (total_push + spill_frame_size) % 16;
        if misalign == 0 { spill_frame_size } else { spill_frame_size + (16 - misalign) }
    };

    let mut epilogue_label = asm.create_label();

    if !naked {
        // ── Prologue ──
        for &reg in &callee_saved {
            asm.push(preg_to_reg64(reg))?;
        }
        asm.push(SIM_BASE)?;
        if frame_size > 0 {
            asm.sub(rsp, frame_size as i32)?;
        }
        asm.mov(SIM_BASE, rdi)?;
    }

    // ── Blocks ──
    // In naked mode, reorder blocks so that Return-ending blocks come last.
    // This enables fall-through to the next EU (no jmp needed).
    let block_order: Vec<usize> = (0..func.blocks.len()).collect();
    let mut fell_through = false; // track if previous block fell through (needs label spacing)
    for (order_idx, &bi) in block_order.iter().enumerate() {
        let block = &func.blocks[bi];
        let next_block_id = block_order.get(order_idx + 1)
            .map(|&next_bi| func.blocks[next_bi].id);

        if fell_through {
            // Previous block fell through — this label follows directly.
            // No nop needed because the fall-through IS the label position.
            fell_through = false;
        }

        let label = block_labels.get_mut(&block.id).unwrap();
        asm.set_label(label)?;

        // Pre-scan: detect Cmp+Branch fusion opportunity.
        // If the instruction immediately before Branch is Cmp/CmpImm,
        // and the cmp result is only used by the Branch, we can fuse
        // into cmp + jcc (skipping setcc + movzx + test).
        let fused_cmp: Option<VReg> = if block.insts.len() >= 2 {
            if let Some(MInst::Branch { cond, true_bb, false_bb }) = block.terminator() {
                let pre = &block.insts[block.insts.len() - 2];
                let is_cmp = pre.def() == Some(*cond)
                    && matches!(pre, MInst::Cmp { .. } | MInst::CmpImm { .. });
                let no_phi_targets = !func.blocks.iter().any(|b| {
                    (b.id == *true_bb || b.id == *false_bb) && !b.phis.is_empty()
                });
                if is_cmp && no_phi_targets {
                    let used_elsewhere = block.insts[..block.insts.len() - 2]
                        .iter().any(|i| i.uses().contains(cond));
                    if !used_elsewhere { Some(*cond) } else { None }
                } else { None }
            } else { None }
        } else { None };

        for inst in block.insts.iter() {
            match inst {
                MInst::Return => {
                    asm.xor(eax, eax)?;
                    if naked {
                        // Naked: emit special byte sequence as placeholder.
                        // INT3 (0xCC) × 5 — easy to find, never generated normally.
                        asm.int3()?;
                        asm.int3()?;
                        asm.int3()?;
                        asm.int3()?;
                        asm.int3()?;
                        naked_return_labels.push((CodeLabel::default(), false));
                    } else {
                        asm.jmp(epilogue_label)?;
                    }
                }
                MInst::ReturnError { code } => {
                    asm.mov(eax, *code as u32)?;
                    if naked {
                        asm.int3()?;
                        asm.int3()?;
                        asm.int3()?;
                        asm.int3()?;
                        asm.int3()?;
                        naked_return_labels.push((CodeLabel::default(), true));
                    } else {
                        asm.jmp(epilogue_label)?;
                    }
                }
                MInst::Jump { target } => {
                    emit_phi_moves_for_target(&mut asm, block.id, *target, func, assignment)?;
                    if next_block_id == Some(*target) {
                        // Fall-through (nop for label spacing if block is otherwise empty)
                        asm.nop()?;
                    } else {
                        let label = block_labels.get_mut(target).unwrap();
                        asm.jmp(*label)?;
                    }
                }
                MInst::Branch { cond, true_bb, false_bb } => {
                    if fused_cmp == Some(*cond) {
                        // Fused Cmp+Branch: emit cmp + jcc directly
                        let cmp_inst = &block.insts[block.insts.len() - 2];
                        let kind = match cmp_inst {
                            MInst::Cmp { lhs, rhs, kind, .. } => {
                                let l = preg_to_reg64(resolve(assignment, *lhs));
                                let r = preg_to_reg64(resolve(assignment, *rhs));
                                asm.cmp(l, r)?;
                                *kind
                            }
                            MInst::CmpImm { lhs, imm, kind, .. } => {
                                let l = preg_to_reg64(resolve(assignment, *lhs));
                                if *imm == 0 && matches!(kind, CmpKind::Eq | CmpKind::Ne) {
                                    asm.test(l, l)?;
                                } else {
                                    asm.cmp(l, *imm)?;
                                }
                                *kind
                            }
                            _ => unreachable!(),
                        };
                        let true_label = block_labels.get_mut(true_bb).unwrap();
                        emit_jcc(&mut asm, *true_label, kind)?;
                        if next_block_id == Some(*false_bb) {
                            // Fall-through: jcc handles true path, false falls through
                        } else {
                            let false_label = block_labels.get_mut(false_bb).unwrap();
                            asm.jmp(*false_label)?;
                        }
                    } else {
                    let c = preg_to_reg64(resolve(assignment, *cond));
                    asm.test(c, c)?;
                    let true_has_phis = func.blocks.iter()
                        .find(|b| b.id == *true_bb)
                        .is_some_and(|b| !b.phis.is_empty());
                    let false_has_phis = func.blocks.iter()
                        .find(|b| b.id == *false_bb)
                        .is_some_and(|b| !b.phis.is_empty());

                    if !true_has_phis && !false_has_phis {
                        let true_label = block_labels.get_mut(true_bb).unwrap();
                        asm.jne(*true_label)?;
                        if next_block_id != Some(*false_bb) {
                            let false_label = block_labels.get_mut(false_bb).unwrap();
                            asm.jmp(*false_label)?;
                        }
                    } else {
                        let mut true_phi_label = asm.create_label();
                        asm.jne(true_phi_label)?;
                        emit_phi_moves_for_target(&mut asm, block.id, *false_bb, func, assignment)?;
                        let false_label = block_labels.get_mut(false_bb).unwrap();
                        asm.jmp(*false_label)?;
                        asm.set_label(&mut true_phi_label)?;
                        emit_phi_moves_for_target(&mut asm, block.id, *true_bb, func, assignment)?;
                        let true_label = block_labels.get_mut(true_bb).unwrap();
                        asm.jmp(*true_label)?;
                    }
                    } // end else (non-fused branch)
                }
                MInst::UDiv { dst, lhs, rhs } => {
                    emit_divrem(&mut asm, assignment, *dst, *lhs, *rhs, DivOp::Div)?;
                }
                MInst::URem { dst, lhs, rhs } => {
                    emit_divrem(&mut asm, assignment, *dst, *lhs, *rhs, DivOp::Rem)?;
                }
                _ => {
                    // Skip Cmp/CmpImm if it's fused with the following Branch
                    if let Some(fc) = fused_cmp {
                        if inst.def() == Some(fc) {
                            continue;
                        }
                    }
                    emit_inst(&mut asm, inst, assignment, func)?;
                }
            }
        }
    }

    if !naked {
        // ── Epilogue ──
        asm.set_label(&mut epilogue_label)?;
        if frame_size > 0 {
            asm.add(rsp, frame_size as i32)?;
        }
        asm.pop(SIM_BASE)?;
        for &reg in callee_saved.iter().rev() {
            asm.pop(preg_to_reg64(reg))?;
        }
        asm.ret()?;
    }

    let code = asm.assemble(0x0)?;

    // In naked mode, find return placeholders (5 × INT3 = CC CC CC CC CC).
    // Replace each with a jmp rel32 placeholder (E9 00 00 00 00) and record offset.
    if naked {
        let mut code = code;
        let mut label_idx = 0;
        let mut pos = 0;
        while pos + 5 <= code.len() {
            if code[pos..pos+5] == [0xCC, 0xCC, 0xCC, 0xCC, 0xCC] {
                // Replace INT3×5 with jmp rel32 placeholder
                code[pos] = 0xE9;
                code[pos+1] = 0x00;
                code[pos+2] = 0x00;
                code[pos+3] = 0x00;
                code[pos+4] = 0x00;
                if label_idx < naked_return_labels.len() {
                    let is_error = naked_return_labels[label_idx].1;
                    return_patches.push((pos, is_error));
                    label_idx += 1;
                }
                pos += 5;
            } else {
                pos += 1;
            }
        }
        return Ok(EmitResult { code, frame_size, return_patches });
    }

    Ok(EmitResult { code, frame_size, return_patches })
}

fn emit_inst(
    asm: &mut CodeAssembler,
    inst: &MInst,
    assignment: &AssignmentMap,
    func: &MFunction,
) -> Result<(), IcedError> {
    match inst {
        MInst::Mov { dst, src } => {
            let d_preg = resolve(assignment, *dst);
            let s_preg = resolve(assignment, *src);
            if d_preg != s_preg {
                if func.is_narrow32(*src) {
                    asm.mov(preg_to_reg32(d_preg), preg_to_reg32(s_preg))?;
                } else {
                    asm.mov(preg_to_reg64(d_preg), preg_to_reg64(s_preg))?;
                }
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
        // Use 32-bit registers when all operands are known to be ≤ 32 bits.
        MInst::Add { dst, lhs, rhs } => {
            let n32 = func.is_narrow32(*dst);
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Add, n32)?;
        }
        MInst::Sub { dst, lhs, rhs } => {
            let n32 = func.is_narrow32(*dst);
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Sub, n32)?;
        }
        MInst::Mul { dst, lhs, rhs } => {
            let n32 = func.is_narrow32(*dst);
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Mul, n32)?;
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
                if rax != l { asm.mov(rax, l)?; }
                asm.mul(r)?;
            }
            if d != rdx { asm.mov(d, rdx)?; }
        }
        MInst::And { dst, lhs, rhs } => {
            let n32 = func.is_narrow32(*dst);
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::And, n32)?;
        }
        MInst::Or { dst, lhs, rhs } => {
            let n32 = func.is_narrow32(*dst);
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Or, n32)?;
        }
        MInst::Xor { dst, lhs, rhs } => {
            let n32 = func.is_narrow32(*dst);
            emit_binop_rr(asm, assignment, *dst, *lhs, *rhs, BinOp::Xor, n32)?;
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

        // Immediate ALU — use 32-bit regs when result fits
        MInst::AndImm { dst, src, imm } => {
            if func.is_narrow32(*dst) && *imm <= u32::MAX as u64 {
                let d = preg_to_reg32(resolve(assignment, *dst));
                let s = preg_to_reg32(resolve(assignment, *src));
                if d != s { asm.mov(d, s)?; }
                asm.and(d, *imm as i32)?;
            } else {
                let d = preg_to_reg64(resolve(assignment, *dst));
                let s = preg_to_reg64(resolve(assignment, *src));
                if d != s { asm.mov(d, s)?; }
                emit_and_imm64(asm, d, *imm)?;
            }
        }
        MInst::OrImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s { asm.mov(d, s)?; }
            emit_or_imm64(asm, d, *imm)?;
        }
        MInst::ShrImm { dst, src, imm } => {
            if func.is_narrow32(*src) {
                let d = preg_to_reg32(resolve(assignment, *dst));
                let s = preg_to_reg32(resolve(assignment, *src));
                if d != s { asm.mov(d, s)?; }
                asm.shr(d, *imm as u32)?;
            } else {
                let d = preg_to_reg64(resolve(assignment, *dst));
                let s = preg_to_reg64(resolve(assignment, *src));
                if d != s { asm.mov(d, s)?; }
                asm.shr(d, *imm as u32)?;
            }
        }
        MInst::ShlImm { dst, src, imm } => {
            if func.is_narrow32(*dst) {
                let d = preg_to_reg32(resolve(assignment, *dst));
                let s = preg_to_reg32(resolve(assignment, *src));
                if d != s { asm.mov(d, s)?; }
                asm.shl(d, *imm as u32)?;
            } else {
                let d = preg_to_reg64(resolve(assignment, *dst));
                let s = preg_to_reg64(resolve(assignment, *src));
                if d != s { asm.mov(d, s)?; }
                asm.shl(d, *imm as u32)?;
            }
        }
        MInst::SarImm { dst, src, imm } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s { asm.mov(d, s)?; }
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
            if d != s {
                asm.mov(d, s)?;
            }
            asm.sub(d, *imm)?;
        }

        MInst::Cmp { dst, lhs, rhs, kind } => {
            let l = preg_to_reg64(resolve(assignment, *lhs));
            let r = preg_to_reg64(resolve(assignment, *rhs));
            asm.cmp(l, r)?;
            let d8 = preg_to_reg8(resolve(assignment, *dst));
            let d32 = preg_to_reg32(resolve(assignment, *dst));
            emit_setcc(asm, d8, *kind)?;
            asm.movzx(d32, d8)?;
        }
        MInst::CmpImm { dst, lhs, imm, kind } => {
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

        MInst::Pext { dst, src, mask } => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            let m = preg_to_reg64(resolve(assignment, *mask));
            asm.pext(d, s, m)?;
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

        // Branch and Jump are handled in the main emit loop (with phi moves).
        MInst::Branch { .. } | MInst::Jump { .. } => {
            unreachable!("Branch/Jump should be handled in main emit loop");
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

/// Emit setcc instruction for a comparison kind.
fn emit_jcc(asm: &mut CodeAssembler, label: CodeLabel, kind: CmpKind) -> Result<(), IcedError> {
    match kind {
        CmpKind::Eq => asm.je(label),
        CmpKind::Ne => asm.jne(label),
        CmpKind::LtU => asm.jb(label),
        CmpKind::LtS => asm.jl(label),
        CmpKind::LeU => asm.jbe(label),
        CmpKind::LeS => asm.jle(label),
        CmpKind::GtU => asm.ja(label),
        CmpKind::GtS => asm.jg(label),
        CmpKind::GeU => asm.jae(label),
        CmpKind::GeS => asm.jge(label),
    }
}

fn emit_setcc(asm: &mut CodeAssembler, d8: AsmRegister8, kind: CmpKind) -> Result<(), IcedError> {
    match kind {
        CmpKind::Eq => asm.sete(d8),
        CmpKind::Ne => asm.setne(d8),
        CmpKind::LtU => asm.setb(d8),
        CmpKind::LtS => asm.setl(d8),
        CmpKind::LeU => asm.setbe(d8),
        CmpKind::LeS => asm.setle(d8),
        CmpKind::GtU => asm.seta(d8),
        CmpKind::GtS => asm.setg(d8),
        CmpKind::GeU => asm.setae(d8),
        CmpKind::GeS => asm.setge(d8),
    }
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
    narrow32: bool,
) -> Result<(), IcedError> {
    if narrow32 {
        emit_binop_rr_32(asm, assignment, dst, lhs, rhs, op)
    } else {
        emit_binop_rr_64(asm, assignment, dst, lhs, rhs, op)
    }
}

fn emit_binop_rr_64(
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

    let (eff_l, eff_r) = if d == r && d != l {
        if op.is_commutative() {
            (r, l)
        } else {
            asm.neg(d)?;
            asm.add(d, l)?;
            return Ok(());
        }
    } else {
        if d != l { asm.mov(d, l)?; }
        (d, r)
    };

    let _ = eff_l;
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

fn emit_binop_rr_32(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: BinOp,
) -> Result<(), IcedError> {
    let dp = resolve(assignment, dst);
    let lp = resolve(assignment, lhs);
    let rp = resolve(assignment, rhs);
    let d = preg_to_reg32(dp);
    let l = preg_to_reg32(lp);
    let r = preg_to_reg32(rp);

    let (eff_l, eff_r) = if d == r && d != l {
        if op.is_commutative() {
            (r, l)
        } else {
            // Non-commutative (sub): d == rhs, d != lhs.
            asm.neg(d)?;
            asm.add(d, l)?;
            return Ok(());
        }
    } else {
        if d != l { asm.mov(d, l)?; }
        (d, r)
    };

    let _ = eff_l;
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
fn emit_or_imm64(
    asm: &mut CodeAssembler,
    d: AsmRegister64,
    imm: u64,
) -> Result<(), IcedError> {
    if imm == 0 {
        return Ok(());
    }
    let signed = imm as i64;
    // ISel must decompose 64-bit OR immediates into LoadImm + Or.
    assert!(
        signed >= i32::MIN as i64 && signed <= i32::MAX as i64,
        "OrImm {imm:#x} exceeds i32: ISel should emit LoadImm + Or instead"
    );
    asm.or(d, signed as i32)?;
    Ok(())
}

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
        panic!(
            "AndImm {imm:#x} exceeds u32: ISel should emit LoadImm + And instead"
        );
    }
    Ok(())
}

// ────────────────────────────────────────────────────────────────
// Multi-EU chained emission
// ────────────────────────────────────────────────────────────────

/// Compile multiple EUs into a single JIT function.
///
/// Each EU is independently compiled (ISel + regalloc + emit) producing
/// Compile multiple EUs into a single merged function.
///
/// Instead of compiling each EU independently and concatenating machine code,
/// this merges all EUs into one MFunction at the MIR level. This enables:
/// - Single prologue/epilogue (no redundant push/pop between EUs)
/// - Cross-EU register allocation (values survive EU boundaries in registers)
/// - Cross-EU MIR optimization (CSE, constant propagation across EU boundaries)
pub fn emit_chained_eus(
    units: &[crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &crate::backend::MemoryLayout,
    four_state: bool,
) -> Result<Vec<u8>, IcedError> {
    use super::{isel, regalloc};

    if units.len() == 1 {
        let mut mfunc = isel::lower_execution_unit(&units[0], layout, four_state);
        super::mir_opt::optimize(&mut mfunc);
        let ra = regalloc::run_regalloc(&mut mfunc);
        let result = emit(&mfunc, &ra.assignment, ra.spill_frame_size)?;
        return Ok(result.code);
    }

    // SIR-level EU merge: combine all EUs into one SIR EU
    let (mut merged_sir, sir_boundaries) = crate::ir::merge_sir_eus(units);

    // Re-run SIR optimization on merged EU (cross-EU optimization)
    let boundary_blocks = sir_boundaries;
    crate::optimizer::coalescing::pass_eliminate_working_round_trip::eliminate_working_round_trip(
        &mut merged_sir, &boundary_blocks,
    );
    crate::optimizer::coalescing::commit_ops::inline_commit_forwarding(&mut merged_sir);

    // Single ISel + optimization + regalloc on the merged EU
    let mut merged = isel::lower_execution_unit(&merged_sir, layout, four_state);

    super::mir_opt::optimize(&mut merged);

    // Step 3: Unified regalloc
    let ra = regalloc::run_regalloc(&mut merged);

    // Step 4: Compute frame layout
    let callee_saved = used_callee_saved(&ra.assignment);
    let callee_push_size = (callee_saved.len() as u32) * 8;
    let total_push = callee_push_size + 8; // +8 for R15
    let frame_size = {
        let misalign = (total_push + ra.spill_frame_size) % 16;
        if misalign == 0 { ra.spill_frame_size } else { ra.spill_frame_size + (16 - misalign) }
    };

    // Step 5: Emit merged body (naked — no prologue/epilogue)
    let body = emit_naked(&merged, &ra.assignment, frame_size)?;

    // Step 6: Build shared prologue + epilogue
    let prologue = {
        let mut asm = CodeAssembler::new(64)?;
        for &reg in &callee_saved { asm.push(preg_to_reg64(reg))?; }
        asm.push(SIM_BASE)?;
        if frame_size > 0 { asm.sub(rsp, frame_size as i32)?; }
        asm.mov(SIM_BASE, rdi)?;
        asm.assemble(0x0)?
    };
    let epilogue = {
        let mut asm = CodeAssembler::new(64)?;
        if frame_size > 0 { asm.add(rsp, frame_size as i32)?; }
        asm.pop(SIM_BASE)?;
        for &reg in callee_saved.iter().rev() { asm.pop(preg_to_reg64(reg))?; }
        asm.ret()?;
        asm.assemble(0x0)?
    };

    // Step 7: Assemble [prologue][body][epilogue] and patch return jmps
    let mut combined: Vec<u8> = Vec::new();
    combined.extend_from_slice(&prologue);
    let body_offset = combined.len();
    combined.extend_from_slice(&body.code);
    let epilogue_offset = combined.len();
    combined.extend_from_slice(&epilogue);

    for &(patch_off, _) in &body.return_patches {
        let abs = body_offset + patch_off;
        let rel = (epilogue_offset as i64) - (abs as i64 + 5);
        combined[abs + 1..abs + 5].copy_from_slice(&(rel as i32).to_le_bytes());
    }

    Ok(combined)
}

/// Merge multiple MFunctions into a single MFunction.
/// Each EU's Return is replaced with Jump to the next EU's entry block.
/// VRegs and BlockIds are renumbered to avoid conflicts.
fn merge_mfunctions(funcs: &mut [MFunction]) -> MFunction {
    use super::mir::*;

    let mut merged_vregs = VRegAllocator::new();
    let mut merged_spill_descs: Vec<SpillDesc> = Vec::new();
    let mut merged_blocks: Vec<MBlock> = Vec::new();

    // Pre-compute offsets
    let mut vreg_offsets: Vec<u32> = Vec::new();
    let mut block_offsets: Vec<u32> = Vec::new();
    let mut vreg_offset = 0u32;
    let mut block_offset = 0u32;

    for func in funcs.iter() {
        vreg_offsets.push(vreg_offset);
        block_offsets.push(block_offset);
        vreg_offset += func.vregs.count();
        // Use max block id + 1 (not blocks.len()) to avoid ID collisions
        // when SIR BlockIds are non-contiguous.
        let max_bid = func.blocks.iter().map(|b| b.id.0).max().unwrap_or(0);
        block_offset += max_bid + 1;
    }

    // Allocate all VRegs in merged allocator
    for _ in 0..vreg_offset {
        merged_vregs.alloc();
    }

    // Entry block IDs for each EU (after renumbering)
    let eu_entry_blocks: Vec<BlockId> = funcs.iter().enumerate()
        .map(|(i, f)| {
            let first_block_id = f.blocks.first().map(|b| b.id.0).unwrap_or(0);
            BlockId(first_block_id + block_offsets[i])
        })
        .collect();

    for (eu_idx, func) in funcs.iter().enumerate() {
        let vo = vreg_offsets[eu_idx];
        let bo = block_offsets[eu_idx];
        let is_last = eu_idx == funcs.len() - 1;
        let next_entry = if !is_last { Some(eu_entry_blocks[eu_idx + 1]) } else { None };

        // Copy spill descs with VReg offset
        for desc in &func.spill_descs {
            merged_spill_descs.push(desc.clone());
        }

        // Process blocks
        for block in &func.blocks {
            let new_block_id = BlockId(block.id.0 + bo);
            let mut new_block = MBlock::new(new_block_id);

            // Renumber phis
            for phi in &block.phis {
                let new_dst = VReg(phi.dst.0 + vo);
                let new_sources: Vec<(BlockId, VReg)> = phi.sources.iter()
                    .map(|(bid, vreg)| (BlockId(bid.0 + bo), VReg(vreg.0 + vo)))
                    .collect();
                new_block.phis.push(PhiNode { dst: new_dst, sources: new_sources });
            }

            // Renumber instructions
            for inst in &block.insts {
                let new_inst = renumber_inst(inst, vo, bo, is_last, next_entry);
                new_block.insts.push(new_inst);
            }

            merged_blocks.push(new_block);
        }
    }

    let mut merged = MFunction::new(merged_vregs, merged_spill_descs);
    merged.blocks = merged_blocks;
    merged
}

/// Renumber VRegs and BlockIds in an instruction.
/// For non-final EUs, Return becomes Jump to next EU entry.
fn renumber_inst(
    inst: &MInst,
    vo: u32,    // VReg offset
    bo: u32,    // BlockId offset
    is_last: bool,
    next_entry: Option<BlockId>,
) -> MInst {
    use super::mir::*;

    // Helper closures
    let v = |vreg: VReg| VReg(vreg.0 + vo);
    let b = |bid: BlockId| BlockId(bid.0 + bo);

    match inst {
        // Control flow: renumber block targets, handle Return
        MInst::Return => {
            if is_last {
                MInst::Return
            } else {
                MInst::Jump { target: next_entry.unwrap() }
            }
        }
        MInst::ReturnError { code } => MInst::ReturnError { code: *code },
        MInst::Jump { target } => MInst::Jump { target: b(*target) },
        MInst::Branch { cond, true_bb, false_bb } => MInst::Branch {
            cond: v(*cond), true_bb: b(*true_bb), false_bb: b(*false_bb),
        },

        // Data movement
        MInst::Mov { dst, src } => MInst::Mov { dst: v(*dst), src: v(*src) },
        MInst::LoadImm { dst, value } => MInst::LoadImm { dst: v(*dst), value: *value },
        MInst::Load { dst, base, offset, size } => MInst::Load { dst: v(*dst), base: *base, offset: *offset, size: *size },
        MInst::Store { base, offset, src, size } => MInst::Store { base: *base, offset: *offset, src: v(*src), size: *size },
        MInst::LoadIndexed { dst, base, offset, index, size } => MInst::LoadIndexed { dst: v(*dst), base: *base, offset: *offset, index: v(*index), size: *size },
        MInst::StoreIndexed { base, offset, index, src, size } => MInst::StoreIndexed { base: *base, offset: *offset, index: v(*index), src: v(*src), size: *size },

        // ALU reg-reg
        MInst::Add { dst, lhs, rhs } => MInst::Add { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::Sub { dst, lhs, rhs } => MInst::Sub { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::Mul { dst, lhs, rhs } => MInst::Mul { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::UMulHi { dst, lhs, rhs } => MInst::UMulHi { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::And { dst, lhs, rhs } => MInst::And { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::Or { dst, lhs, rhs } => MInst::Or { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::Xor { dst, lhs, rhs } => MInst::Xor { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::Shr { dst, lhs, rhs } => MInst::Shr { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::Shl { dst, lhs, rhs } => MInst::Shl { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::Sar { dst, lhs, rhs } => MInst::Sar { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::UDiv { dst, lhs, rhs } => MInst::UDiv { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },
        MInst::URem { dst, lhs, rhs } => MInst::URem { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs) },

        // ALU with immediate
        MInst::AndImm { dst, src, imm } => MInst::AndImm { dst: v(*dst), src: v(*src), imm: *imm },
        MInst::OrImm { dst, src, imm } => MInst::OrImm { dst: v(*dst), src: v(*src), imm: *imm },
        MInst::ShrImm { dst, src, imm } => MInst::ShrImm { dst: v(*dst), src: v(*src), imm: *imm },
        MInst::ShlImm { dst, src, imm } => MInst::ShlImm { dst: v(*dst), src: v(*src), imm: *imm },
        MInst::SarImm { dst, src, imm } => MInst::SarImm { dst: v(*dst), src: v(*src), imm: *imm },
        MInst::AddImm { dst, src, imm } => MInst::AddImm { dst: v(*dst), src: v(*src), imm: *imm },
        MInst::SubImm { dst, src, imm } => MInst::SubImm { dst: v(*dst), src: v(*src), imm: *imm },

        // Comparison
        MInst::Cmp { dst, lhs, rhs, kind } => MInst::Cmp { dst: v(*dst), lhs: v(*lhs), rhs: v(*rhs), kind: *kind },
        MInst::CmpImm { dst, lhs, imm, kind } => MInst::CmpImm { dst: v(*dst), lhs: v(*lhs), imm: *imm, kind: *kind },

        // Unary
        MInst::BitNot { dst, src } => MInst::BitNot { dst: v(*dst), src: v(*src) },
        MInst::Neg { dst, src } => MInst::Neg { dst: v(*dst), src: v(*src) },
        MInst::Popcnt { dst, src } => MInst::Popcnt { dst: v(*dst), src: v(*src) },
        MInst::Pext { dst, src, mask } => MInst::Pext { dst: v(*dst), src: v(*src), mask: v(*mask) },

        // Select
        MInst::Select { dst, cond, true_val, false_val } => MInst::Select {
            dst: v(*dst), cond: v(*cond), true_val: v(*true_val), false_val: v(*false_val),
        },
    }
}
