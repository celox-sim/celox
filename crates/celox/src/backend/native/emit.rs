//! x86-64 code emission: MIR + physical register assignment → machine code.
//!
//! Uses iced-x86's CodeAssembler for instruction encoding.
//! ABI: System V AMD64 — sim state base in RDI (moved to R15 in prologue).
//! Function signature: `fn(unified_mem: *mut u8) -> i64`

use std::collections::HashMap;

use iced_x86::BlockEncoderOptions;
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
        PhysReg::RBP => rbp,
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
        PhysReg::RBP => ebp,
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
        PhysReg::RBP => bp,
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
        PhysReg::RBP => bpl,
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

fn mem_operand_ptr(ptr: AsmRegister64, offset: i32) -> AsmMemoryOperand {
    ptr + offset
}

fn mem_operand_ptr_indexed(
    ptr: AsmRegister64,
    offset: i32,
    index: AsmRegister64,
) -> AsmMemoryOperand {
    ptr + index + offset
}

// ────────────────────────────────────────────────────────────────
// Callee-saved register tracking
// ────────────────────────────────────────────────────────────────

const CALLEE_SAVED: &[PhysReg] = &[
    PhysReg::RBX,
    PhysReg::RBP,
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
    /// Machine-code offsets for MIR basic-block entry labels.
    pub block_offsets: Vec<(BlockId, u64)>,
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
    let Some(target_block) = target_block else {
        return Ok(());
    };

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

    if copies.is_empty() {
        return Ok(());
    }

    // Preserve true parallel-copy semantics even for cycles. Phi edges are
    // cold compared with straight-line eval code, so prefer correctness and
    // simple code over clever xchg scheduling.
    for (_, src) in &copies {
        asm.push(preg_to_reg64(*src))?;
    }
    for (dst, _) in copies.iter().rev() {
        asm.pop(preg_to_reg64(*dst))?;
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
    let mut block_labels: HashMap<BlockId, CodeLabel> = HashMap::new();
    for block in &func.blocks {
        block_labels.insert(block.id, asm.create_label());
    }

    let callee_saved = used_callee_saved(assignment);
    let callee_push_size = (callee_saved.len() as u32) * 8;
    let total_push = callee_push_size + 8;
    let frame_size = {
        let misalign = (total_push + spill_frame_size) % 16;
        if misalign == 0 {
            spill_frame_size
        } else {
            spill_frame_size + (16 - misalign)
        }
    };

    let mut epilogue_label = asm.create_label();
    let use_counts = count_vreg_uses(func);

    // ── Prologue ──
    {
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
    let block_order: Vec<usize> = (0..func.blocks.len()).collect();
    let mut fell_through = false; // track if previous block fell through (needs label spacing)
    for (order_idx, &bi) in block_order.iter().enumerate() {
        let block = &func.blocks[bi];
        let next_block_id = block_order
            .get(order_idx + 1)
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
            if let Some(MInst::Branch {
                cond,
                true_bb,
                false_bb,
            }) = block.terminator()
            {
                let pre = &block.insts[block.insts.len() - 2];
                let is_cmp = pre.def() == Some(*cond)
                    && matches!(pre, MInst::Cmp { .. } | MInst::CmpImm { .. });
                let no_phi_targets = !func
                    .blocks
                    .iter()
                    .any(|b| (b.id == *true_bb || b.id == *false_bb) && !b.phis.is_empty());
                if is_cmp && no_phi_targets {
                    let used_elsewhere = block.insts[..block.insts.len() - 2]
                        .iter()
                        .any(|i| i.uses().contains(cond));
                    if !used_elsewhere { Some(*cond) } else { None }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let mut inst_idx = 0usize;
        while inst_idx < block.insts.len() {
            let inst = &block.insts[inst_idx];
            match inst {
                MInst::Return => {
                    asm.xor(eax, eax)?;
                    asm.jmp(epilogue_label)?;
                }
                MInst::ReturnError { code } => {
                    asm.mov(eax, *code as u32)?;
                    asm.jmp(epilogue_label)?;
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
                MInst::Branch {
                    cond,
                    true_bb,
                    false_bb,
                } => {
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
                        let true_has_phis = func
                            .blocks
                            .iter()
                            .find(|b| b.id == *true_bb)
                            .is_some_and(|b| !b.phis.is_empty());
                        let false_has_phis = func
                            .blocks
                            .iter()
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
                            emit_phi_moves_for_target(
                                &mut asm, block.id, *false_bb, func, assignment,
                            )?;
                            let false_label = block_labels.get_mut(false_bb).unwrap();
                            asm.jmp(*false_label)?;
                            asm.set_label(&mut true_phi_label)?;
                            emit_phi_moves_for_target(
                                &mut asm, block.id, *true_bb, func, assignment,
                            )?;
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
                            inst_idx += 1;
                            continue;
                        }
                    }
                    if inst_idx + 1 < block.insts.len()
                        && fused_cmp.is_none_or(|fc| block.insts[inst_idx + 1].def() != Some(fc))
                        && try_emit_stack_reload_fold(
                            &mut asm,
                            inst,
                            &block.insts[inst_idx + 1],
                            &use_counts,
                            assignment,
                            func,
                        )?
                    {
                        inst_idx += 2;
                        continue;
                    }
                    emit_inst(&mut asm, inst, assignment, func)?;
                }
            }
            inst_idx += 1;
        }
    }

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

    let result = asm.assemble_options(0x0, BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS)?;
    let block_offsets = func
        .blocks
        .iter()
        .filter_map(|block| {
            let label = block_labels.get(&block.id)?;
            result.label_ip(label).ok().map(|ip| (block.id, ip))
        })
        .collect();
    Ok(EmitResult {
        code: result.inner.code_buffer,
        frame_size,
        block_offsets,
    })
}

fn count_vreg_uses(func: &MFunction) -> HashMap<VReg, usize> {
    let mut counts = HashMap::new();
    for block in &func.blocks {
        for phi in &block.phis {
            for (_, src) in &phi.sources {
                *counts.entry(*src).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for vreg in inst.uses() {
                *counts.entry(vreg).or_default() += 1;
            }
        }
    }
    counts
}

fn try_emit_stack_reload_fold(
    asm: &mut CodeAssembler,
    inst: &MInst,
    next: &MInst,
    use_counts: &HashMap<VReg, usize>,
    assignment: &AssignmentMap,
    func: &MFunction,
) -> Result<bool, IcedError> {
    let MInst::Load {
        dst,
        base: BaseReg::StackFrame,
        offset,
        size: OpSize::S64,
    } = inst
    else {
        return Ok(false);
    };
    if use_counts.get(dst).copied().unwrap_or(0) != 1 || !next.uses().contains(dst) {
        return Ok(false);
    }
    emit_inst_with_stack_mem(asm, next, *dst, *offset, assignment, func)
}

fn emit_inst_with_stack_mem(
    asm: &mut CodeAssembler,
    inst: &MInst,
    stack_vreg: VReg,
    stack_offset: i32,
    assignment: &AssignmentMap,
    func: &MFunction,
) -> Result<bool, IcedError> {
    match inst {
        MInst::Mov { dst, src } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            Ok(true)
        }
        MInst::Add { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            func,
            BinOp::Add,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Sub { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            func,
            BinOp::Sub,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Mul { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            func,
            BinOp::Mul,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::And { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            func,
            BinOp::And,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Or { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            func,
            BinOp::Or,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Xor { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            func,
            BinOp::Xor,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::AndImm { dst, src, imm } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            emit_and_imm64(asm, d, *imm)?;
            Ok(true)
        }
        MInst::OrImm { dst, src, imm } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            emit_or_imm64(asm, d, *imm)?;
            Ok(true)
        }
        MInst::AddImm { dst, src, imm } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            asm.add(d, *imm)?;
            Ok(true)
        }
        MInst::SubImm { dst, src, imm } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            asm.sub(d, *imm)?;
            Ok(true)
        }
        MInst::ShrImm { dst, src, imm } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            asm.shr(d, *imm as u32)?;
            Ok(true)
        }
        MInst::ShlImm { dst, src, imm } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            asm.shl(d, *imm as u32)?;
            Ok(true)
        }
        MInst::SarImm { dst, src, imm } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            asm.sar(d, *imm as u32)?;
            Ok(true)
        }
        MInst::Cmp {
            dst,
            lhs,
            rhs,
            kind,
        } if *lhs == stack_vreg || *rhs == stack_vreg => {
            let mem = qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset));
            if *lhs == stack_vreg {
                let r = preg_to_reg64(resolve(assignment, *rhs));
                asm.cmp(mem, r)?;
            } else {
                let l = preg_to_reg64(resolve(assignment, *lhs));
                asm.cmp(l, mem)?;
            }
            let d8 = preg_to_reg8(resolve(assignment, *dst));
            let d32 = preg_to_reg32(resolve(assignment, *dst));
            emit_setcc(asm, d8, *kind)?;
            asm.movzx(d32, d8)?;
            Ok(true)
        }
        MInst::CmpImm {
            dst,
            lhs,
            imm,
            kind,
        } if *lhs == stack_vreg => {
            asm.cmp(
                qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)),
                *imm,
            )?;
            let d8 = preg_to_reg8(resolve(assignment, *dst));
            let d32 = preg_to_reg32(resolve(assignment, *dst));
            emit_setcc(asm, d8, *kind)?;
            asm.movzx(d32, d8)?;
            Ok(true)
        }
        MInst::BitNot { dst, src } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            asm.not(d)?;
            Ok(true)
        }
        MInst::Neg { dst, src } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            asm.neg(d)?;
            Ok(true)
        }
        MInst::Popcnt { dst, src } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.popcnt(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            Ok(true)
        }
        MInst::Bsr { dst, src } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.bsr(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            Ok(true)
        }
        MInst::Select {
            dst,
            cond,
            true_val,
            false_val,
        } => emit_select_stack_mem(
            asm,
            assignment,
            *dst,
            *cond,
            *true_val,
            *false_val,
            stack_vreg,
            stack_offset,
        ),
        _ => Ok(false),
    }
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

        MInst::Load {
            dst,
            base,
            offset,
            size,
        } => {
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

        MInst::Store {
            base,
            offset,
            src,
            size,
        } => {
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

        MInst::MemCopy {
            src_offset,
            dst_offset,
            byte_len,
        } => {
            if *byte_len == 0 {
                return Ok(());
            }
            asm.push(rax)?;
            asm.push(rcx)?;
            asm.push(rsi)?;
            asm.push(rdi)?;
            asm.lea(rsi, mem_operand(BaseReg::SimState, *src_offset))?;
            asm.lea(rdi, mem_operand(BaseReg::SimState, *dst_offset))?;
            let qwords = byte_len / 8;
            let rem = byte_len % 8;
            if qwords > 0 {
                let mut loop_label = asm.create_label();
                let mut done_label = asm.create_label();
                asm.mov(rcx, qwords as i64)?;
                asm.set_label(&mut loop_label)?;
                asm.cmp(rcx, 0)?;
                asm.je(done_label)?;
                asm.mov(rax, qword_ptr(rsi))?;
                asm.mov(qword_ptr(rdi), rax)?;
                asm.add(rsi, 8)?;
                asm.add(rdi, 8)?;
                asm.dec(rcx)?;
                asm.jmp(loop_label)?;
                asm.set_label(&mut done_label)?;
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
            asm.pop(rdi)?;
            asm.pop(rsi)?;
            asm.pop(rcx)?;
            asm.pop(rax)?;
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
            size,
        } => {
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
        } => {
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
                if d != s {
                    asm.mov(d, s)?;
                }
                asm.and(d, *imm as i32)?;
            } else {
                let d = preg_to_reg64(resolve(assignment, *dst));
                let s = preg_to_reg64(resolve(assignment, *src));
                if d != s {
                    asm.mov(d, s)?;
                }
                emit_and_imm64(asm, d, *imm)?;
            }
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
            if func.is_narrow32(*src) {
                let d = preg_to_reg32(resolve(assignment, *dst));
                let s = preg_to_reg32(resolve(assignment, *src));
                if d != s {
                    asm.mov(d, s)?;
                }
                asm.shr(d, *imm as u32)?;
            } else {
                let d = preg_to_reg64(resolve(assignment, *dst));
                let s = preg_to_reg64(resolve(assignment, *src));
                if d != s {
                    asm.mov(d, s)?;
                }
                asm.shr(d, *imm as u32)?;
            }
        }
        MInst::ShlImm { dst, src, imm } => {
            if func.is_narrow32(*dst) {
                let d = preg_to_reg32(resolve(assignment, *dst));
                let s = preg_to_reg32(resolve(assignment, *src));
                if d != s {
                    asm.mov(d, s)?;
                }
                asm.shl(d, *imm as u32)?;
            } else {
                let d = preg_to_reg64(resolve(assignment, *dst));
                let s = preg_to_reg64(resolve(assignment, *src));
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
            if d != s {
                asm.mov(d, s)?;
            }
            asm.sub(d, *imm)?;
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
            let mut done = asm.create_label();
            asm.bsr(d, s)?;
            asm.jne(done)?;
            asm.mov(d, *zero_value as i64)?;
            asm.set_label(&mut done)?;
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

    // Divisor cannot be read from RAX/RDX because div consumes RDX:RAX.
    // Use a stack copy instead of an unmodeled scratch register clobber.
    let rhs_on_stack = r == rax || r == rdx;
    if rhs_on_stack {
        asm.push(r)?;
    }

    if l != rax {
        asm.mov(rax, l)?;
    }
    asm.xor(edx, edx)?;
    if rhs_on_stack {
        asm.div(qword_ptr(rsp))?;
        asm.add(rsp, 8)?;
    } else {
        asm.div(r)?;
    }

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
        matches!(
            self,
            BinOp::Add | BinOp::Mul | BinOp::And | BinOp::Or | BinOp::Xor
        )
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

fn emit_binop_stack_mem(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    func: &MFunction,
    op: BinOp,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    stack_vreg: VReg,
    stack_offset: i32,
) -> Result<bool, IcedError> {
    let narrow32 = func.is_narrow32(dst);
    if rhs == stack_vreg {
        let other = lhs;
        if narrow32 {
            let d = preg_to_reg32(resolve(assignment, dst));
            let o = preg_to_reg32(resolve(assignment, other));
            if d != o {
                asm.mov(d, o)?;
            }
            let mem = dword_ptr(mem_operand(BaseReg::StackFrame, stack_offset));
            match op {
                BinOp::Add => asm.add(d, mem)?,
                BinOp::Sub => asm.sub(d, mem)?,
                BinOp::Mul => asm.imul_2(d, mem)?,
                BinOp::And => asm.and(d, mem)?,
                BinOp::Or => asm.or(d, mem)?,
                BinOp::Xor => asm.xor(d, mem)?,
            }
        } else {
            let d = preg_to_reg64(resolve(assignment, dst));
            let o = preg_to_reg64(resolve(assignment, other));
            if d != o {
                asm.mov(d, o)?;
            }
            let mem = qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset));
            match op {
                BinOp::Add => asm.add(d, mem)?,
                BinOp::Sub => asm.sub(d, mem)?,
                BinOp::Mul => asm.imul_2(d, mem)?,
                BinOp::And => asm.and(d, mem)?,
                BinOp::Or => asm.or(d, mem)?,
                BinOp::Xor => asm.xor(d, mem)?,
            }
        }
        return Ok(true);
    }

    if lhs == stack_vreg && op.is_commutative() {
        let other = rhs;
        if narrow32 {
            let d = preg_to_reg32(resolve(assignment, dst));
            let o = preg_to_reg32(resolve(assignment, other));
            if d != o {
                asm.mov(d, o)?;
            }
            let mem = dword_ptr(mem_operand(BaseReg::StackFrame, stack_offset));
            match op {
                BinOp::Add => asm.add(d, mem)?,
                BinOp::Mul => asm.imul_2(d, mem)?,
                BinOp::And => asm.and(d, mem)?,
                BinOp::Or => asm.or(d, mem)?,
                BinOp::Xor => asm.xor(d, mem)?,
                BinOp::Sub => unreachable!(),
            }
        } else {
            let d = preg_to_reg64(resolve(assignment, dst));
            let o = preg_to_reg64(resolve(assignment, other));
            if d != o {
                asm.mov(d, o)?;
            }
            let mem = qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset));
            match op {
                BinOp::Add => asm.add(d, mem)?,
                BinOp::Mul => asm.imul_2(d, mem)?,
                BinOp::And => asm.and(d, mem)?,
                BinOp::Or => asm.or(d, mem)?,
                BinOp::Xor => asm.xor(d, mem)?,
                BinOp::Sub => unreachable!(),
            }
        }
        return Ok(true);
    }

    Ok(false)
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
        if d != l {
            asm.mov(d, l)?;
        }
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
        if d != l {
            asm.mov(d, l)?;
        }
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

fn emit_select_stack_mem(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    cond: VReg,
    true_val: VReg,
    false_val: VReg,
    stack_vreg: VReg,
    stack_offset: i32,
) -> Result<bool, IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    if cond == stack_vreg {
        asm.cmp(qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)), 0)?;
        let tv = preg_to_reg64(resolve(assignment, true_val));
        let fv = preg_to_reg64(resolve(assignment, false_val));
        if d == tv {
            asm.cmove(d, fv)?;
        } else {
            if d != fv {
                asm.mov(d, fv)?;
            }
            asm.cmovne(d, tv)?;
        }
        return Ok(true);
    }

    let c = preg_to_reg64(resolve(assignment, cond));
    asm.test(c, c)?;
    if true_val == stack_vreg {
        let fv = preg_to_reg64(resolve(assignment, false_val));
        if d != fv {
            asm.mov(d, fv)?;
        }
        asm.cmovne(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
        return Ok(true);
    }
    if false_val == stack_vreg {
        let tv = preg_to_reg64(resolve(assignment, true_val));
        if d != tv {
            asm.mov(d, tv)?;
        }
        asm.cmove(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
        return Ok(true);
    }

    Ok(false)
}

/// Emit AND with a potentially 64-bit immediate.
/// Uses the most efficient encoding available.
fn emit_or_imm64(asm: &mut CodeAssembler, d: AsmRegister64, imm: u64) -> Result<(), IcedError> {
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

fn emit_and_imm64(asm: &mut CodeAssembler, d: AsmRegister64, imm: u64) -> Result<(), IcedError> {
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
            _ if d == rbp => ebp,
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
        panic!("AndImm {imm:#x} exceeds u32: ISel should emit LoadImm + And instead");
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
    label: &str,
) -> Result<EmitResult, IcedError> {
    use super::{isel, regalloc};
    let timing = std::env::var_os("CELOX_PHASE_TIMING").is_some();
    let mir_stats = std::env::var_os("CELOX_MIR_STATS").is_some();
    let verify_mir = cfg!(debug_assertions) || std::env::var_os("CELOX_MIR_VERIFY").is_some();
    let total_start = timing.then(crate::timing::now);

    // SIR-level EU merge: combine all EUs into one SIR EU
    let merge_start = timing.then(crate::timing::now);
    let (sir_eu, _sir_boundaries) = if units.len() > 1 {
        let (mut merged, boundaries) = crate::ir::merge_sir_eus(units);
        // Cross-EU SIR optimization
        crate::optimizer::coalescing::pass_eliminate_working_round_trip::eliminate_working_round_trip(
            &mut merged,
            &boundaries,
        );
        crate::optimizer::coalescing::commit_ops::inline_commit_forwarding(&mut merged);
        (merged, boundaries)
    } else {
        (units[0].clone(), vec![])
    };
    if let Some(start) = merge_start {
        let sir_insts: usize = sir_eu
            .blocks
            .values()
            .map(|block| block.instructions.len())
            .sum();
        eprintln!(
            "[native-timing] emit_chained merge eus={} sir_blocks={} sir_insts={} elapsed={:?}",
            units.len(),
            sir_eu.blocks.len(),
            sir_insts,
            start.elapsed()
        );
    }
    if timing {
        log_sir_width_stats(&sir_eu);
    }

    // Single ISel + optimize + regalloc + emit
    let isel_start = timing.then(crate::timing::now);
    let mut mfunc = isel::lower_execution_unit(&sir_eu, layout, four_state);
    if let Some(start) = isel_start {
        eprintln!(
            "[native-timing] emit_chained isel mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    dump_native_block_context(label, "after_isel", &sir_eu, &mfunc);
    if verify_mir {
        if timing {
            eprintln!("[native-timing] emit_chained verify after_isel label={label}");
        }
        mfunc.verify();
    }
    let legalize_start = timing.then(crate::timing::now);
    super::mir_legalize::legalize(&mut mfunc);
    if let Some(start) = legalize_start {
        eprintln!(
            "[native-timing] emit_chained legalize mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    if verify_mir {
        if timing {
            eprintln!("[native-timing] emit_chained verify after_legalize label={label}");
        }
        mfunc.verify();
    }
    let opt_start = timing.then(crate::timing::now);
    super::mir_opt::optimize(&mut mfunc);
    if let Some(start) = opt_start {
        eprintln!(
            "[native-timing] emit_chained mir_opt label={label} mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    if mir_stats {
        log_mir_stats(label, "after_mir_opt", &mfunc);
    }
    if std::env::var_os("CELOX_MIR_BLOCK_STATS").is_some() {
        log_mir_block_stats(label, "after_mir_opt", &mfunc);
    }
    dump_native_block_context(label, "after_mir_opt", &sir_eu, &mfunc);
    if verify_mir {
        if timing {
            eprintln!("[native-timing] emit_chained verify after_mir_opt label={label}");
        }
        mfunc.verify();
    }
    let regalloc_start = timing.then(crate::timing::now);
    let ra = regalloc::run_regalloc_with_label(&mut mfunc, label);
    if let Some(start) = regalloc_start {
        eprintln!(
            "[native-timing] emit_chained regalloc mir_blocks={} mir_insts={} vregs={} spill_frame={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            ra.spill_frame_size,
            start.elapsed()
        );
    }
    let post_regalloc_start = timing.then(crate::timing::now);
    super::mir_opt::post_regalloc_peephole(&mut mfunc);
    if let Some(start) = post_regalloc_start {
        eprintln!(
            "[native-timing] emit_chained post_regalloc_peephole mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    if mir_stats {
        log_mir_stats(label, "after_regalloc", &mfunc);
    }
    if std::env::var_os("CELOX_MIR_BLOCK_STATS").is_some() {
        log_mir_block_stats(label, "after_regalloc", &mfunc);
    }
    dump_native_block_context(label, "after_regalloc", &sir_eu, &mfunc);
    let emit_start = timing.then(crate::timing::now);
    let result = emit(&mfunc, &ra.assignment, ra.spill_frame_size)?;
    if let Some(start) = emit_start {
        eprintln!(
            "[native-timing] emit_chained emit bytes={} elapsed={:?}",
            result.code.len(),
            start.elapsed()
        );
    }
    if let Some(start) = total_start {
        eprintln!(
            "[native-timing] emit_chained total elapsed={:?}",
            start.elapsed()
        );
    }
    Ok(result)
}

fn mir_inst_count(func: &super::mir::MFunction) -> usize {
    func.blocks
        .iter()
        .map(|block| block.phis.len() + block.insts.len())
        .sum()
}

fn log_mir_stats(label: &str, stage: &str, func: &super::mir::MFunction) {
    let mut phi = 0usize;
    let mut mov = 0usize;
    let mut imm = 0usize;
    let mut load_sim = 0usize;
    let mut load_stack = 0usize;
    let mut load_ptr = 0usize;
    let mut store_sim = 0usize;
    let mut store_stack = 0usize;
    let mut store_ptr = 0usize;
    let mut indexed_load = 0usize;
    let mut indexed_store = 0usize;
    let mut memcopy = 0usize;
    let mut alu = 0usize;
    let mut alu_imm = 0usize;
    let mut cmp = 0usize;
    let mut div_rem = 0usize;
    let mut bit_ops = 0usize;
    let mut select = 0usize;
    let mut branch = 0usize;
    let mut jump = 0usize;
    let mut ret = 0usize;

    for block in &func.blocks {
        phi += block.phis.len();
        for inst in &block.insts {
            match inst {
                MInst::Mov { .. } => mov += 1,
                MInst::LoadImm { .. } => imm += 1,
                MInst::Load { base, .. } => match base {
                    BaseReg::SimState => load_sim += 1,
                    BaseReg::StackFrame => load_stack += 1,
                },
                MInst::Store { base, .. } => match base {
                    BaseReg::SimState => store_sim += 1,
                    BaseReg::StackFrame => store_stack += 1,
                },
                MInst::LoadPtr { .. } => load_ptr += 1,
                MInst::StorePtr { .. } | MInst::ReleaseStorePtr { .. } => store_ptr += 1,
                MInst::LoadIndexed { .. } | MInst::LoadPtrIndexed { .. } => indexed_load += 1,
                MInst::StoreIndexed { .. }
                | MInst::StorePtrIndexed { .. }
                | MInst::ReleaseStorePtrIndexed { .. } => indexed_store += 1,
                MInst::MemCopy { .. } => memcopy += 1,
                MInst::Add { .. }
                | MInst::Sub { .. }
                | MInst::Mul { .. }
                | MInst::UMulHi { .. }
                | MInst::And { .. }
                | MInst::Or { .. }
                | MInst::Xor { .. }
                | MInst::Shr { .. }
                | MInst::Shl { .. }
                | MInst::Sar { .. } => alu += 1,
                MInst::AndImm { .. }
                | MInst::OrImm { .. }
                | MInst::ShrImm { .. }
                | MInst::ShlImm { .. }
                | MInst::SarImm { .. }
                | MInst::AddImm { .. }
                | MInst::SubImm { .. } => alu_imm += 1,
                MInst::Cmp { .. } | MInst::CmpImm { .. } => cmp += 1,
                MInst::UDiv { .. } | MInst::URem { .. } => div_rem += 1,
                MInst::BitNot { .. }
                | MInst::Neg { .. }
                | MInst::Popcnt { .. }
                | MInst::Bsr { .. }
                | MInst::BsrOr { .. }
                | MInst::Pext { .. }
                | MInst::Pdep { .. } => bit_ops += 1,
                MInst::Select { .. } => select += 1,
                MInst::Branch { .. } => branch += 1,
                MInst::Jump { .. } => jump += 1,
                MInst::Return | MInst::ReturnError { .. } => ret += 1,
            }
        }
    }

    eprintln!(
        "[native-mir-stats] label={label} stage={stage} phi={phi} mov={mov} imm={imm} load_sim={load_sim} load_stack={load_stack} load_ptr={load_ptr} store_sim={store_sim} store_stack={store_stack} store_ptr={store_ptr} indexed_load={indexed_load} indexed_store={indexed_store} memcopy={memcopy} alu={alu} alu_imm={alu_imm} cmp={cmp} div_rem={div_rem} bit_ops={bit_ops} select={select} branch={branch} jump={jump} ret={ret}"
    );
}

fn log_mir_block_stats(label: &str, stage: &str, func: &super::mir::MFunction) {
    let mut blocks = func
        .blocks
        .iter()
        .map(|block| {
            let insts = block.phis.len() + block.insts.len();
            let mut load_stack = 0usize;
            let mut store_stack = 0usize;
            let mut imm = 0usize;
            let mut alu = 0usize;
            let mut alu_imm = 0usize;
            for inst in &block.insts {
                match inst {
                    MInst::Load {
                        base: BaseReg::StackFrame,
                        ..
                    } => load_stack += 1,
                    MInst::Store {
                        base: BaseReg::StackFrame,
                        ..
                    } => store_stack += 1,
                    MInst::LoadImm { .. } => imm += 1,
                    MInst::Add { .. }
                    | MInst::Sub { .. }
                    | MInst::Mul { .. }
                    | MInst::UMulHi { .. }
                    | MInst::And { .. }
                    | MInst::Or { .. }
                    | MInst::Xor { .. }
                    | MInst::Shr { .. }
                    | MInst::Shl { .. }
                    | MInst::Sar { .. } => alu += 1,
                    MInst::AndImm { .. }
                    | MInst::OrImm { .. }
                    | MInst::ShrImm { .. }
                    | MInst::ShlImm { .. }
                    | MInst::SarImm { .. }
                    | MInst::AddImm { .. }
                    | MInst::SubImm { .. } => alu_imm += 1,
                    _ => {}
                }
            }
            (
                insts,
                block.id.0,
                block.phis.len(),
                block.insts.len(),
                load_stack,
                store_stack,
                imm,
                alu,
                alu_imm,
            )
        })
        .collect::<Vec<_>>();
    blocks.sort_unstable_by(|a, b| b.cmp(a));
    for (rank, (total, block_id, phis, insts, load_stack, store_stack, imm, alu, alu_imm)) in
        blocks.into_iter().take(10).enumerate()
    {
        eprintln!(
            "[native-mir-block-stats] label={label} stage={stage} rank={} block={} total={} phis={} insts={} load_stack={} store_stack={} imm={} alu={} alu_imm={}",
            rank + 1,
            block_id,
            total,
            phis,
            insts,
            load_stack,
            store_stack,
            imm,
            alu,
            alu_imm
        );
    }
}

fn dump_native_block_context(
    label: &str,
    stage: &str,
    eu: &crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>,
    func: &super::mir::MFunction,
) {
    let Some(raw) = std::env::var_os("CELOX_NATIVE_DUMP_BLOCK") else {
        return;
    };
    if let Some(raw_stage) = std::env::var_os("CELOX_NATIVE_DUMP_STAGE") {
        if raw_stage != stage {
            return;
        }
    } else if stage != "after_isel" {
        return;
    }
    let Some(block_id) = raw.to_string_lossy().parse::<u32>().ok() else {
        return;
    };
    let dump_sir = std::env::var_os("CELOX_NATIVE_DUMP_SIR").is_none_or(|raw| raw != "0");
    let mir_limit = std::env::var_os("CELOX_NATIVE_DUMP_MIR_LIMIT")
        .and_then(|raw| raw.to_string_lossy().parse::<usize>().ok())
        .unwrap_or(64);
    let sir_id = crate::ir::BlockId(block_id as usize);
    eprintln!("[native-dump] label={label} stage={stage} block={block_id}");
    if dump_sir {
        if let Some(block) = eu.blocks.get(&sir_id) {
            eprintln!("[native-dump] SIR:\n{block}");
            dump_sir_operand_defs(eu, block);
        } else {
            eprintln!("[native-dump] SIR block b{block_id} not found");
        }
    }
    if let Some(block) = func
        .blocks
        .iter()
        .find(|block| block.id == super::mir::BlockId(block_id))
    {
        eprintln!(
            "[native-dump] MIR b{} phis={} insts={}",
            block.id.0,
            block.phis.len(),
            block.insts.len()
        );
        for phi in &block.phis {
            let sources = phi
                .sources
                .iter()
                .map(|(pred, src)| format!("b{}:{}", pred.0, src))
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!("  {} = phi({sources})", phi.dst);
        }
        for (idx, inst) in block.insts.iter().enumerate().take(mir_limit) {
            eprintln!("  {idx}: {inst}");
        }
        if block.insts.len() > mir_limit {
            eprintln!("  ... {} more insts", block.insts.len() - mir_limit);
        }
    } else {
        eprintln!("[native-dump] MIR block b{block_id} not found");
    }
}

fn dump_sir_operand_defs(
    eu: &crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>,
    block: &crate::ir::BasicBlock<crate::ir::RegionedAbsoluteAddr>,
) {
    let mut regs = Vec::new();
    for inst in &block.instructions {
        collect_sir_inst_uses(inst, &mut regs);
    }
    regs.sort();
    regs.dedup();
    for reg in regs {
        let mut found = false;
        for other in eu.blocks.values() {
            if other.params.contains(&reg) {
                eprintln!("  [sir-def] r{} is param of b{}", reg.0, other.id.0);
                found = true;
            }
            for (idx, inst) in other.instructions.iter().enumerate() {
                if sir_inst_def(inst) == Some(reg) {
                    eprintln!(
                        "  [sir-def] r{} defined at b{} inst {}: {}",
                        reg.0, other.id.0, idx, inst
                    );
                    found = true;
                }
            }
        }
        if !found {
            eprintln!("  [sir-def] r{} has no SIR definition", reg.0);
        }
    }
}

fn sir_inst_def(
    inst: &crate::ir::SIRInstruction<crate::ir::RegionedAbsoluteAddr>,
) -> Option<crate::ir::RegisterId> {
    use crate::ir::SIRInstruction;
    match inst {
        SIRInstruction::Imm(dst, _)
        | SIRInstruction::Load(dst, _, _, _)
        | SIRInstruction::Binary(dst, _, _, _)
        | SIRInstruction::Unary(dst, _, _)
        | SIRInstruction::Concat(dst, _)
        | SIRInstruction::Slice(dst, _, _, _)
        | SIRInstruction::Mux(dst, _, _, _) => Some(*dst),
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => None,
    }
}

fn collect_sir_inst_uses(
    inst: &crate::ir::SIRInstruction<crate::ir::RegionedAbsoluteAddr>,
    out: &mut Vec<crate::ir::RegisterId>,
) {
    use crate::ir::SIRInstruction;
    match inst {
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            out.push(*lhs);
            out.push(*rhs);
        }
        SIRInstruction::Unary(_, _, src)
        | SIRInstruction::Store(_, _, _, src, _, _)
        | SIRInstruction::Slice(_, src, _, _) => out.push(*src),
        SIRInstruction::Commit(..) | SIRInstruction::Imm(..) | SIRInstruction::Load(..) => {}
        SIRInstruction::Concat(_, args) | SIRInstruction::RuntimeEvent { args, .. } => {
            out.extend(args.iter().copied());
        }
        SIRInstruction::Mux(_, cond, then_val, else_val) => {
            out.push(*cond);
            out.push(*then_val);
            out.push(*else_val);
        }
        SIRInstruction::CombCaptureEvent { args, .. } => {
            out.extend(args.iter().copied());
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            out.push(*old);
            out.push(*new);
        }
    }
}

fn log_sir_width_stats(eu: &crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>) {
    use crate::ir::{RegisterType, SIRInstruction};

    let mut max_reg_width = 0usize;
    let mut regs_gt_1024 = 0usize;
    for reg_ty in eu.register_map.values() {
        let width = match reg_ty {
            RegisterType::Logic { width } | RegisterType::Bit { width, .. } => *width,
        };
        max_reg_width = max_reg_width.max(width);
        if width > 1024 {
            regs_gt_1024 += 1;
        }
    }

    let mut max_inst_width = 0usize;
    let mut wide_loads = 0usize;
    let mut wide_stores = 0usize;
    let mut wide_commits = 0usize;
    let mut wide_slices = 0usize;
    let mut est_chunks = 0usize;
    let mut examples = Vec::new();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            match inst {
                SIRInstruction::Load(_, addr, offset, width) => {
                    max_inst_width = max_inst_width.max(*width);
                    est_chunks += width.div_ceil(64);
                    if *width > 1024 {
                        wide_loads += 1;
                        if examples.len() < 8 {
                            examples.push(format!(
                                "Load addr={addr:?} offset={offset:?} width={width}"
                            ));
                        }
                    }
                }
                SIRInstruction::Store(addr, offset, width, _, _, _) => {
                    max_inst_width = max_inst_width.max(*width);
                    est_chunks += width.div_ceil(64);
                    if *width > 1024 {
                        wide_stores += 1;
                        if examples.len() < 8 {
                            examples.push(format!(
                                "Store addr={addr:?} offset={offset:?} width={width}"
                            ));
                        }
                    }
                }
                SIRInstruction::Commit(src, dst, offset, width, _) => {
                    max_inst_width = max_inst_width.max(*width);
                    est_chunks += width.div_ceil(64);
                    if *width > 1024 {
                        wide_commits += 1;
                        if examples.len() < 8 {
                            examples.push(format!(
                                "Commit src={src:?} dst={dst:?} offset={offset:?} width={width}"
                            ));
                        }
                    }
                }
                SIRInstruction::Slice(_, _, offset, width) => {
                    max_inst_width = max_inst_width.max(*width);
                    est_chunks += width.div_ceil(64);
                    if *width > 1024 {
                        wide_slices += 1;
                        if examples.len() < 8 {
                            examples.push(format!("Slice offset={offset} width={width}"));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    eprintln!(
        "[native-timing] sir_width_stats regs={} regs_gt_1024={} max_reg_width={} max_inst_width={} wide_loads={} wide_stores={} wide_commits={} wide_slices={} est_width_chunks={}",
        eu.register_map.len(),
        regs_gt_1024,
        max_reg_width,
        max_inst_width,
        wide_loads,
        wide_stores,
        wide_commits,
        wide_slices,
        est_chunks
    );
    for example in examples {
        eprintln!("[native-timing] sir_width_example {example}");
    }
}
