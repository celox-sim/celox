//! x86-64 code emission: MIR + physical register assignment → machine code.
//!
//! Uses iced-x86's CodeAssembler for instruction encoding.
//! ABI: System V AMD64 at the external boundary. On supported x86-64 hosts,
//! generated code temporarily uses an otherwise available segment base for
//! simulation-state/allocator-arena addressing, leaving every non-stack GPR
//! available to allocation. Other hosts reserve R15 as the state base.
//! Function signature: `fn(unified_mem: *mut u8) -> i64`

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fmt;

use iced_x86::BlockEncoderOptions;
use iced_x86::code_asm::*;

use celox_analysis::cfg::ForwardControlFlowGraph;

use crate::native::features::{StateBaseStrategy, VariableShiftEncoding};
use crate::native::mir::*;
use crate::native::regalloc::assignment::{
    AssignmentMap, PhysReg, PhysRegSet, X86PhysVec, X86VectorLocation, clobbers,
};
use crate::native::ssa_destroy::{
    EdgeCopyPlan, ParallelCopyDestination, ParallelCopyOperation, ParallelCopySource,
    SsaDestructionPlan,
};
use celox_state_layout::{
    STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET, STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET,
    STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
};

pub use crate::native::ssa_destroy::SsaDestructionError;

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
        PhysReg::R15 => r15,
    }
}

fn x86_vec_to_xmm(register: X86PhysVec) -> AsmRegisterXmm {
    match register.0 {
        0 => xmm0,
        1 => xmm1,
        2 => xmm2,
        3 => xmm3,
        4 => xmm4,
        5 => xmm5,
        6 => xmm6,
        7 => xmm7,
        8 => xmm8,
        9 => xmm9,
        10 => xmm10,
        11 => xmm11,
        12 => xmm12,
        13 => xmm13,
        14 => xmm14,
        15 => xmm15,
        other => panic!("invalid XMM register index {other}"),
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
        PhysReg::R15 => r15d,
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
        PhysReg::R15 => r15w,
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
        PhysReg::R15 => r15b,
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

#[derive(Debug, Clone)]
struct NativeArenaLayout {
    spill_base: i32,
    scratch_base: i32,
    scratch_size: i32,
    loop_gpr_save_base: Option<i32>,
    loop_segment_save: Option<i32>,
    loop_xmm15_save: Option<i32>,
    total_size: u32,
    callee_saved: Vec<PhysReg>,
}

impl NativeArenaLayout {
    fn build(
        func: &MFunction,
        assignment: &AssignmentMap,
        state_size: usize,
        spill_frame_size: u32,
        state_base: StateBaseStrategy,
        tick_loop: bool,
    ) -> Result<Self, EmitInputError> {
        fn align16(value: usize) -> Option<usize> {
            value.checked_add(15).map(|value| value & !15)
        }

        let spill_base = align16(state_size).ok_or_else(|| {
            EmitInputError::new(
                "EMIT.NATIVE_ARENA_RANGE",
                None,
                None,
                None,
                "simulation-state size overflows native arena layout",
            )
        })?;
        let scratch_base = align16(
            spill_base
                .checked_add(spill_frame_size as usize)
                .ok_or_else(|| {
                    EmitInputError::new(
                        "EMIT.NATIVE_ARENA_RANGE",
                        None,
                        None,
                        None,
                        "spill frame overflows native arena layout",
                    )
                })?,
        )
        .ok_or_else(|| {
            EmitInputError::new(
                "EMIT.NATIVE_ARENA_RANGE",
                None,
                None,
                None,
                "spill-frame alignment overflows native arena layout",
            )
        })?;
        // Four qwords cover the largest fixed-register save set used by one
        // inline memory pseudo. The same instruction-local area is reused by
        // div/shift and parallel-copy cycle breaking.
        let scratch_size = align16(4usize * 8).ok_or_else(|| {
            EmitInputError::new(
                "EMIT.NATIVE_ARENA_RANGE",
                None,
                None,
                None,
                "instruction scratch area overflows native arena layout",
            )
        })?;
        let callee_saved =
            used_callee_saved(func, assignment, state_base == StateBaseStrategy::R15);
        let loop_save_base = scratch_base.checked_add(scratch_size).ok_or_else(|| {
            EmitInputError::new(
                "EMIT.NATIVE_ARENA_RANGE",
                None,
                None,
                None,
                "native arena size overflows",
            )
        })?;
        let loop_segment_save = loop_save_base
            .checked_add(callee_saved.len().checked_mul(8).ok_or_else(|| {
                EmitInputError::new(
                    "EMIT.NATIVE_ARENA_RANGE",
                    None,
                    None,
                    None,
                    "native loop GPR save area overflows",
                )
            })?)
            .ok_or_else(|| {
                EmitInputError::new(
                    "EMIT.NATIVE_ARENA_RANGE",
                    None,
                    None,
                    None,
                    "native loop segment save area overflows",
                )
            })?;
        let loop_xmm15_save = loop_segment_save.checked_add(8).ok_or_else(|| {
            EmitInputError::new(
                "EMIT.NATIVE_ARENA_RANGE",
                None,
                None,
                None,
                "native loop XMM15 save area overflows",
            )
        })?;
        let total_size = align16(
            loop_save_base
                .checked_add(
                    usize::from(tick_loop)
                        * (8 + callee_saved.len().checked_mul(8).ok_or_else(|| {
                            EmitInputError::new(
                                "EMIT.NATIVE_ARENA_RANGE",
                                None,
                                None,
                                None,
                                "native loop save area overflows",
                            )
                        })? + usize::from(cfg!(target_os = "windows")) * 16),
                )
                .ok_or_else(|| {
                    EmitInputError::new(
                        "EMIT.NATIVE_ARENA_RANGE",
                        None,
                        None,
                        None,
                        "native loop save area overflows",
                    )
                })?,
        )
        .ok_or_else(|| {
            EmitInputError::new(
                "EMIT.NATIVE_ARENA_RANGE",
                None,
                None,
                None,
                "native arena alignment overflows",
            )
        })?;

        let to_i32 = |value: usize, what: &'static str| {
            i32::try_from(value).map_err(|_| {
                EmitInputError::new(
                    "EMIT.NATIVE_ARENA_RANGE",
                    None,
                    None,
                    None,
                    format!("{what} exceeds signed 32-bit x86 displacement"),
                )
            })
        };
        Ok(Self {
            spill_base: to_i32(spill_base, "spill base")?,
            scratch_base: to_i32(scratch_base, "scratch base")?,
            scratch_size: to_i32(scratch_size, "scratch size")?,
            loop_gpr_save_base: tick_loop
                .then(|| to_i32(loop_save_base, "native loop GPR save base"))
                .transpose()?,
            loop_segment_save: tick_loop
                .then(|| to_i32(loop_segment_save, "native loop segment save"))
                .transpose()?,
            loop_xmm15_save: (tick_loop && cfg!(target_os = "windows"))
                .then(|| to_i32(loop_xmm15_save, "native loop XMM15 save"))
                .transpose()?,
            total_size: u32::try_from(total_size).map_err(|_| {
                EmitInputError::new(
                    "EMIT.NATIVE_ARENA_RANGE",
                    None,
                    None,
                    None,
                    "native arena size exceeds u32",
                )
            })?,
            callee_saved,
        })
    }
}

fn saved_gpr_xmm(index: usize) -> AsmRegisterXmm {
    match index {
        0 => xmm9,
        1 => xmm10,
        2 => xmm11,
        3 => xmm12,
        4 => xmm13,
        5 => xmm14,
        _ => unreachable!("x86-64 has at most six allocatable callee-saved GPRs"),
    }
}

/// A post-allocation cache for the hottest ordinary qword spill slots.
///
/// Explicit vector values and this scalar-home cache share one physical XMM
/// inventory. Vector assignment owns registers first; the cache may use only
/// registers which remain unassigned and are not clobbered by any pseudo in
/// the body. A native tick loop can use XMM0..XMM14. Standalone functions keep
/// XMM9..XMM14 for ABI-boundary GPR saves.
#[derive(Debug, Clone, Copy, Default)]
struct SpillRegisterCache {
    entries: [Option<(i32, X86PhysVec)>; 15],
}

impl SpillRegisterCache {
    fn register(self, offset: i32) -> Option<AsmRegisterXmm> {
        self.entries
            .iter()
            .flatten()
            .find_map(|&(candidate, register)| {
                (candidate == offset).then(|| x86_vec_to_xmm(register))
            })
    }
}

fn ranges_overlap(left_offset: i32, left_size: u32, right_offset: i32, right_size: u32) -> bool {
    let left_start = i64::from(left_offset);
    let left_end = left_start + i64::from(left_size);
    let right_start = i64::from(right_offset);
    let right_end = right_start + i64::from(right_size);
    left_start < right_end && right_start < left_end
}

fn select_spill_register_cache(
    func: &MFunction,
    plan: &SsaDestructionPlan,
    assignment: &AssignmentMap,
    tick_loop: bool,
) -> SpillRegisterCache {
    let mut access_counts = HashMap::<i32, usize>::new();
    let mut incompatible_ranges = Vec::<(i32, u32)>::new();
    let mut indexed_stack_access = false;

    for block in &func.blocks {
        for inst in &block.insts {
            match inst {
                MInst::Load {
                    base: BaseReg::StackFrame,
                    offset,
                    size: OpSize::S64,
                    ..
                }
                | MInst::Store {
                    base: BaseReg::StackFrame,
                    offset,
                    size: OpSize::S64,
                    ..
                } => {
                    *access_counts.entry(*offset).or_default() += 1;
                }
                MInst::Load {
                    base: BaseReg::StackFrame,
                    offset,
                    size,
                    ..
                }
                | MInst::Store {
                    base: BaseReg::StackFrame,
                    offset,
                    size,
                    ..
                }
                | MInst::AndStoreImm {
                    base: BaseReg::StackFrame,
                    offset,
                    size,
                    ..
                }
                | MInst::OrStoreImm {
                    base: BaseReg::StackFrame,
                    offset,
                    size,
                    ..
                } => incompatible_ranges.push((*offset, size.bytes())),
                MInst::BranchPred {
                    predicate:
                        BranchPredicate::MemoryNonZero {
                            base: BaseReg::StackFrame,
                            offset,
                            size,
                        },
                    ..
                } => incompatible_ranges.push((*offset, size.bytes())),
                MInst::LoadIndexed {
                    base: BaseReg::StackFrame,
                    ..
                }
                | MInst::StoreIndexed {
                    base: BaseReg::StackFrame,
                    ..
                }
                | MInst::OrStoreIndexed {
                    base: BaseReg::StackFrame,
                    ..
                } => indexed_stack_access = true,
                _ => {}
            }
        }
    }

    // An indexed frame access cannot be proven disjoint from any candidate.
    // Emission verification normally rejects it, but keep selection safe when
    // called independently by unit tests as well.
    if indexed_stack_access {
        return SpillRegisterCache::default();
    }

    let mut edge_slots = HashSet::<i32>::new();
    for edge in plan.edges() {
        for row in &edge.rows {
            if let ParallelCopyDestination::Stack(offset) = row.destination {
                edge_slots.insert(offset);
            }
            if let ParallelCopySource::Stack(offset) = row.source {
                edge_slots.insert(offset);
            }
        }
    }

    let mut candidates = access_counts
        .into_iter()
        .filter(|(offset, count)| {
            *count >= 4
                && !edge_slots.contains(offset)
                && !incompatible_ranges
                    .iter()
                    .any(|&(other_offset, other_size)| {
                        ranges_overlap(*offset, OpSize::S64.bytes(), other_offset, other_size)
                    })
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|(offset, count)| (std::cmp::Reverse(*count), *offset));

    let assigned_vectors = assignment
        .sorted_x86_vectors()
        .into_iter()
        .filter_map(|(_, location)| match location {
            X86VectorLocation::Register(register) => Some(register),
            X86VectorLocation::Stack(_) => None,
        })
        .collect::<HashSet<_>>();
    let vector_scratch_used = assignment
        .sorted_x86_vectors()
        .into_iter()
        .any(|(_, location)| matches!(location, X86VectorLocation::Stack(_)));
    let register_limit = if tick_loop { 15u8 } else { 9u8 };
    let available_registers = (0..register_limit)
        .map(X86PhysVec)
        .filter(|register| !assigned_vectors.contains(register))
        .filter(|register| !(vector_scratch_used && register.0 == 5))
        .filter(|register| {
            !func
                .blocks
                .iter()
                .flat_map(|block| &block.insts)
                .any(|inst| super::x86_slp::clobbers_xmm(inst, *register))
        })
        .collect::<Vec<_>>();

    let mut cache = SpillRegisterCache::default();
    for (entry, ((offset, _), register)) in cache
        .entries
        .iter_mut()
        .zip(candidates.into_iter().zip(available_registers))
    {
        *entry = Some((offset, register));
    }
    cache
}

thread_local! {
    /// Emission-only relocation from logical allocator stack slots into the
    /// per-instance area following simulation state. Native functions are
    /// compiled concurrently, so this context is thread-local rather than
    /// process-global.
    static ACTIVE_SPILL_BASE: Cell<i32> = const { Cell::new(0) };
    static ACTIVE_SCRATCH_BASE: Cell<i32> = const { Cell::new(0) };
    static ACTIVE_STATE_BASE: Cell<StateBaseStrategy> =
        const { Cell::new(StateBaseStrategy::R15) };
}

fn state_base_strategy() -> StateBaseStrategy {
    ACTIVE_STATE_BASE.with(Cell::get)
}

fn physical_offset(base: BaseReg, offset: i32) -> i32 {
    match base {
        BaseReg::SimState => offset,
        BaseReg::StackFrame => ACTIVE_SPILL_BASE.with(|base| {
            base.get()
                .checked_add(offset)
                .expect("verified stack offset fits native arena displacement")
        }),
    }
}

fn mem_operand(base: BaseReg, offset: i32) -> AsmMemoryOperand {
    let offset = physical_offset(base, offset);
    match state_base_strategy() {
        StateBaseStrategy::Fs => ptr(offset).fs(),
        StateBaseStrategy::Gs => ptr(offset).gs(),
        StateBaseStrategy::R15 => r15 + offset,
    }
}

fn scratch_offset(slot: usize) -> i32 {
    ACTIVE_SCRATCH_BASE.with(|base| {
        base.get()
            .checked_add(i32::try_from(slot * 8).expect("scratch slot offset"))
            .expect("scratch slot displacement")
    })
}

fn scratch_operand(slot: usize) -> AsmMemoryOperand {
    let offset = scratch_offset(slot);
    match state_base_strategy() {
        StateBaseStrategy::Fs => ptr(offset).fs(),
        StateBaseStrategy::Gs => ptr(offset).gs(),
        StateBaseStrategy::R15 => r15 + offset,
    }
}

fn mem_operand_indexed(
    base: BaseReg,
    offset: i32,
    index: AsmRegister64,
    scale: u8,
) -> AsmMemoryOperand {
    let offset = physical_offset(base, offset);
    match (state_base_strategy(), scale) {
        (StateBaseStrategy::Fs, 1) => ptr(index + offset).fs(),
        (StateBaseStrategy::Fs, 2) => ptr(index * 2 + offset).fs(),
        (StateBaseStrategy::Fs, 4) => ptr(index * 4 + offset).fs(),
        (StateBaseStrategy::Fs, 8) => ptr(index * 8 + offset).fs(),
        (StateBaseStrategy::Gs, 1) => ptr(index + offset).gs(),
        (StateBaseStrategy::Gs, 2) => ptr(index * 2 + offset).gs(),
        (StateBaseStrategy::Gs, 4) => ptr(index * 4 + offset).gs(),
        (StateBaseStrategy::Gs, 8) => ptr(index * 8 + offset).gs(),
        (StateBaseStrategy::R15, 1) => r15 + index + offset,
        (StateBaseStrategy::R15, 2) => r15 + index * 2 + offset,
        (StateBaseStrategy::R15, 4) => r15 + index * 4 + offset,
        (StateBaseStrategy::R15, 8) => r15 + index * 8 + offset,
        _ => unreachable!("invalid indexed-memory scale {scale}"),
    }
}

fn emit_state_base(asm: &mut CodeAssembler, destination: AsmRegister64) -> Result<(), IcedError> {
    match state_base_strategy() {
        StateBaseStrategy::Fs => asm.rdfsbase(destination),
        StateBaseStrategy::Gs => asm.rdgsbase(destination),
        StateBaseStrategy::R15 => asm.mov(destination, r15),
    }
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

fn emit_direct_memcopy(
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

fn emit_sparse_chunk_copy(
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

fn emit_sparse_commit_worklist(
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

// ────────────────────────────────────────────────────────────────
// Callee-saved register tracking
// ────────────────────────────────────────────────────────────────

const CALLEE_SAVED: &[PhysReg] = &[
    PhysReg::RBX,
    PhysReg::RBP,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
    PhysReg::R15,
];

fn used_callee_saved(
    func: &MFunction,
    assignment: &AssignmentMap,
    reserve_r15_state_base: bool,
) -> Vec<PhysReg> {
    let mut used = PhysRegSet::new();
    for &preg in assignment.map.values() {
        used.insert(preg);
    }
    // Inline pseudos may use fixed scratch registers without defining a
    // VReg.  Their explicit clobber sets participate in allocation, and must
    // also participate in the System V callee-save contract.
    for inst in func.blocks.iter().flat_map(|block| &block.insts) {
        for &preg in clobbers(inst) {
            used.insert(preg);
        }
    }
    if reserve_r15_state_base {
        used.insert(PhysReg::R15);
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
    /// Length of executable text before any RIP-relative constant tables.
    pub text_size: usize,
    /// Stack frame size (bytes) for spill slots, excluding callee-saved pushes.
    pub frame_size: u32,
    /// Total bytes required by simulation state plus the per-function native
    /// spill/scratch/save arena.
    pub required_state_size: u32,
    /// Machine-code offsets for MIR basic-block entry labels.
    pub block_offsets: Vec<(BlockId, u64)>,
}

/// Exact intermediate forms captured while emitting one native function.
///
/// This is populated only for an explicit compilation trace.  Keeping the
/// snapshots inside `emit_chained_eu_groups` guarantees that the dump observes
/// the same merged SIR, MIR, allocation, and machine code as the executable
/// function instead of independently lowering the source execution units.
#[derive(Default)]
pub struct NativeFunctionTrace {
    pub optimized_sir: String,
    pub reactive_graph: String,
    pub state_layout: String,
    pub mir_before_regalloc: String,
    pub mir_after_late_memory_folds: String,
    pub mir_after_scheduling: String,
    pub mir_after_regalloc: String,
    pub register_assignment: String,
    pub spill_frame_size: u32,
    pub disassembly: String,
}

/// Failure of the final MIR/assignment contract required by x86 encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitInputError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub value: Option<VReg>,
    pub message: String,
}

impl EmitInputError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        value: Option<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            value,
            message: message.into(),
        }
    }
}

impl fmt::Display for EmitInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "native emission input [{}]", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        if let Some(value) = self.value {
            write!(formatter, " value={value}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for EmitInputError {}

/// Structured failure while validating SSA destruction or encoding x86-64.
#[derive(Debug)]
pub enum EmitError {
    Mir(crate::native::mir_verify::MirVerifyError),
    Input(EmitInputError),
    SsaDestruction(SsaDestructionError),
    Assembly(IcedError),
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mir(error) => error.fmt(f),
            Self::Input(error) => error.fmt(f),
            Self::SsaDestruction(error) => error.fmt(f),
            Self::Assembly(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for EmitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Mir(error) => Some(error),
            Self::Input(error) => Some(error),
            Self::SsaDestruction(error) => Some(error),
            Self::Assembly(error) => Some(error),
        }
    }
}

impl From<SsaDestructionError> for EmitError {
    fn from(error: SsaDestructionError) -> Self {
        Self::SsaDestruction(error)
    }
}

impl From<EmitInputError> for EmitError {
    fn from(error: EmitInputError) -> Self {
        Self::Input(error)
    }
}

impl From<IcedError> for EmitError {
    fn from(error: IcedError) -> Self {
        Self::Assembly(error)
    }
}

/// Failure while compiling a merged MIR function through allocation and x86
/// encoding.  Allocation diagnostics retain their phase/rule/location rather
/// than being collapsed into a panic.
#[derive(Debug)]
pub enum ChainedEmitError {
    Sir {
        phase: &'static str,
        error: crate::verify::SirVerifyError,
    },
    Mir {
        phase: &'static str,
        error: crate::native::mir_verify::MirVerifyError,
    },
    Analysis {
        phase: &'static str,
        message: String,
    },
    Regalloc(crate::native::regalloc::RegallocError),
    Input(EmitInputError),
    SsaDestruction(SsaDestructionError),
    Assembly(IcedError),
}

impl fmt::Display for ChainedEmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sir { phase, error } => write!(f, "{phase}: {error}"),
            Self::Mir { phase, error } => write!(f, "{phase}: {error}"),
            Self::Analysis { phase, message } => write!(f, "{phase}: {message}"),
            Self::Regalloc(error) => error.fmt(f),
            Self::Input(error) => error.fmt(f),
            Self::SsaDestruction(error) => error.fmt(f),
            Self::Assembly(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ChainedEmitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sir { error, .. } => Some(error),
            Self::Mir { error, .. } => Some(error),
            Self::Analysis { .. } => None,
            Self::Regalloc(error) => Some(error),
            Self::Input(error) => Some(error),
            Self::SsaDestruction(error) => Some(error),
            Self::Assembly(error) => Some(error),
        }
    }
}

impl From<crate::native::regalloc::RegallocError> for ChainedEmitError {
    fn from(error: crate::native::regalloc::RegallocError) -> Self {
        Self::Regalloc(error)
    }
}

impl From<SsaDestructionError> for ChainedEmitError {
    fn from(error: SsaDestructionError) -> Self {
        Self::SsaDestruction(error)
    }
}

impl From<IcedError> for ChainedEmitError {
    fn from(error: IcedError) -> Self {
        Self::Assembly(error)
    }
}

impl From<EmitError> for ChainedEmitError {
    fn from(error: EmitError) -> Self {
        match error {
            EmitError::Mir(error) => Self::Mir {
                phase: "before x86 emission",
                error,
            },
            EmitError::Input(error) => Self::Input(error),
            EmitError::SsaDestruction(error) => Self::SsaDestruction(error),
            EmitError::Assembly(error) => Self::Assembly(error),
        }
    }
}

/// Disassemble the emitted code to a string (NASM syntax).
pub fn disassemble(code: &[u8], base_addr: u64) -> String {
    disassemble_with_block_offsets(code, base_addr, &[])
}

fn disassemble_with_block_offsets(
    code: &[u8],
    base_addr: u64,
    block_offsets: &[(BlockId, u64)],
) -> String {
    use iced_x86::{Decoder, DecoderOptions, Formatter, NasmFormatter};
    let mut decoder = Decoder::with_ip(64, code, base_addr, DecoderOptions::NONE);
    let mut formatter = NasmFormatter::new();
    let mut output = String::new();
    let mut instruction = iced_x86::Instruction::default();
    let mut labels = block_offsets.to_vec();
    labels.sort_unstable_by_key(|(block, offset)| (*offset, *block));
    let mut next_label = 0usize;
    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        let offset = instruction.ip().saturating_sub(base_addr);
        while labels
            .get(next_label)
            .is_some_and(|(_, label_offset)| *label_offset == offset)
        {
            output.push_str(&format!("bb{}:\n", labels[next_label].0.0));
            next_label += 1;
        }
        let mut text = String::new();
        formatter.format(&instruction, &mut text);
        output.push_str(&format!("  {:#010x}  {}\n", instruction.ip(), text));
    }
    output
}

// ────────────────────────────────────────────────────────────────
// Verified parallel-copy lowering
// ────────────────────────────────────────────────────────────────

/// Lower a pre-validated edge plan. This function deliberately has no access
/// to MIR phi nodes or the assignment map: all semantic decisions belong to
/// SSA destruction planning and verification, before x86 encoding starts.
fn emit_parallel_copy_plan(
    asm: &mut CodeAssembler,
    edge: Option<&EdgeCopyPlan>,
) -> Result<(), EmitError> {
    let Some(edge) = edge else {
        return Ok(());
    };

    let mut temporary_live = false;
    for operation in &edge.operations {
        match *operation {
            ParallelCopyOperation::Move {
                destination,
                source,
            } => {
                emit_single_parallel_copy(asm, destination, source, 0)?;
            }
            ParallelCopyOperation::SwapRegisters { left, right } => {
                if temporary_live {
                    return Err(parallel_copy_input_error(
                        "EMIT.PARALLEL_COPY_TEMPORARY",
                        "parallel-copy schedule exchanges registers while a temporary is live",
                    ));
                }
                asm.xchg(preg_to_reg64(left), preg_to_reg64(right))?;
            }
            ParallelCopyOperation::SaveTemporary(location) => {
                if temporary_live {
                    return Err(parallel_copy_input_error(
                        "EMIT.PARALLEL_COPY_TEMPORARY",
                        "parallel-copy schedule nests temporary saves",
                    ));
                }
                match location {
                    ParallelCopyDestination::Register(register) => {
                        asm.mov(qword_ptr(scratch_operand(0)), preg_to_reg64(register))?
                    }
                    ParallelCopyDestination::Stack(slot) => {
                        let offset = checked_parallel_copy_offset(slot, 0)?;
                        asm.movq(xmm0, qword_ptr(mem_operand(BaseReg::StackFrame, offset)))?;
                        asm.movq(qword_ptr(scratch_operand(0)), xmm0)?;
                    }
                }
                temporary_live = true;
            }
            ParallelCopyOperation::RestoreTemporary(location) => {
                if !temporary_live {
                    return Err(parallel_copy_input_error(
                        "EMIT.PARALLEL_COPY_TEMPORARY",
                        "parallel-copy schedule restores an inactive temporary",
                    ));
                }
                match location {
                    ParallelCopyDestination::Register(register) => {
                        asm.mov(preg_to_reg64(register), qword_ptr(scratch_operand(0)))?
                    }
                    ParallelCopyDestination::Stack(slot) => {
                        let offset = checked_parallel_copy_offset(slot, 0)?;
                        asm.movq(xmm0, qword_ptr(scratch_operand(0)))?;
                        asm.movq(qword_ptr(mem_operand(BaseReg::StackFrame, offset)), xmm0)?;
                    }
                }
                temporary_live = false;
            }
        }
    }
    if temporary_live {
        return Err(parallel_copy_input_error(
            "EMIT.PARALLEL_COPY_TEMPORARY",
            "parallel-copy schedule leaves a temporary live",
        ));
    }
    Ok(())
}

fn emit_single_parallel_copy(
    asm: &mut CodeAssembler,
    destination: ParallelCopyDestination,
    source: ParallelCopySource,
    stack_adjustment: i32,
) -> Result<(), EmitError> {
    match (destination, source) {
        (ParallelCopyDestination::Register(dst), ParallelCopySource::Register(src)) => {
            asm.mov(preg_to_reg64(dst), preg_to_reg64(src))?;
        }
        (ParallelCopyDestination::Register(dst), ParallelCopySource::Stack(slot)) => {
            let offset = checked_parallel_copy_offset(slot, stack_adjustment)?;
            asm.mov(
                preg_to_reg64(dst),
                qword_ptr(mem_operand(BaseReg::StackFrame, offset)),
            )?;
        }
        (ParallelCopyDestination::Register(dst), ParallelCopySource::Immediate(value)) => {
            asm.mov(preg_to_reg64(dst), value)?;
        }
        (ParallelCopyDestination::Stack(slot), ParallelCopySource::Register(src)) => {
            let offset = checked_parallel_copy_offset(slot, stack_adjustment)?;
            asm.mov(
                qword_ptr(mem_operand(BaseReg::StackFrame, offset)),
                preg_to_reg64(src),
            )?;
        }
        (ParallelCopyDestination::Stack(dst), ParallelCopySource::Stack(src)) => {
            // XMM0 is not part of the GPR allocator and SSE2 is baseline on
            // x86-64, so it is a safe non-stack scratch for a qword memcopy.
            let source_offset = checked_parallel_copy_offset(src, stack_adjustment)?;
            let destination_offset = checked_parallel_copy_offset(dst, stack_adjustment)?;
            asm.movq(
                xmm0,
                qword_ptr(mem_operand(BaseReg::StackFrame, source_offset)),
            )?;
            asm.movq(
                qword_ptr(mem_operand(BaseReg::StackFrame, destination_offset)),
                xmm0,
            )?;
        }
        (ParallelCopyDestination::Stack(slot), ParallelCopySource::Immediate(value)) => {
            // x86 has no arbitrary imm64-to-memory encoding.  Two independent
            // dword stores avoid borrowing an allocatable GPR or stack scratch.
            let low_offset = checked_parallel_copy_offset(slot, stack_adjustment)?;
            let high_adjustment = stack_adjustment.checked_add(4).ok_or_else(|| {
                parallel_copy_input_error(
                    "EMIT.PARALLEL_COPY_OFFSET",
                    "parallel-copy immediate high-word adjustment exceeds i32",
                )
            })?;
            let high_offset = checked_parallel_copy_offset(slot, high_adjustment)?;
            asm.mov(
                dword_ptr(mem_operand(BaseReg::StackFrame, low_offset)),
                value as u32,
            )?;
            asm.mov(
                dword_ptr(mem_operand(BaseReg::StackFrame, high_offset)),
                (value >> 32) as u32,
            )?;
        }
    }
    Ok(())
}

fn checked_parallel_copy_offset(slot: i32, adjustment: i32) -> Result<i32, EmitError> {
    slot.checked_add(adjustment).ok_or_else(|| {
        parallel_copy_input_error(
            "EMIT.PARALLEL_COPY_OFFSET",
            format!("stack slot {slot} overflows after temporary adjustment {adjustment}"),
        )
    })
}

fn parallel_copy_input_error(rule: &'static str, message: impl Into<String>) -> EmitError {
    EmitInputError::new(rule, None, None, None, message).into()
}

#[derive(Clone, Copy)]
enum EmittedBranchCondition {
    NonZero,
    Compare(CmpKind),
}

fn emit_branch_predicate(
    asm: &mut CodeAssembler,
    predicate: BranchPredicate,
    assignment: &AssignmentMap,
) -> Result<EmittedBranchCondition, IcedError> {
    match predicate {
        BranchPredicate::Compare { lhs, rhs, kind } => {
            asm.cmp(
                preg_to_reg64(resolve(assignment, lhs)),
                preg_to_reg64(resolve(assignment, rhs)),
            )?;
            Ok(EmittedBranchCondition::Compare(kind))
        }
        BranchPredicate::CompareImm { lhs, imm, kind } => {
            let lhs = preg_to_reg64(resolve(assignment, lhs));
            if imm == 0 && matches!(kind, CmpKind::Eq | CmpKind::Ne) {
                asm.test(lhs, lhs)?;
            } else {
                asm.cmp(lhs, imm)?;
            }
            Ok(EmittedBranchCondition::Compare(kind))
        }
        BranchPredicate::MemoryNonZero { base, offset, size } => {
            let memory = mem_operand(base, offset);
            match size {
                OpSize::S8 => asm.cmp(byte_ptr(memory), 0)?,
                OpSize::S16 => asm.cmp(word_ptr(memory), 0)?,
                OpSize::S32 => asm.cmp(dword_ptr(memory), 0)?,
                OpSize::S64 => asm.cmp(qword_ptr(memory), 0)?,
            }
            Ok(EmittedBranchCondition::NonZero)
        }
    }
}

fn emit_condition_jump(
    asm: &mut CodeAssembler,
    label: CodeLabel,
    condition: EmittedBranchCondition,
    jump_when_true: bool,
) -> Result<(), IcedError> {
    match (condition, jump_when_true) {
        (EmittedBranchCondition::NonZero, true) => asm.jne(label),
        (EmittedBranchCondition::NonZero, false) => asm.je(label),
        (EmittedBranchCondition::Compare(kind), true) => emit_jcc(asm, label, kind),
        (EmittedBranchCondition::Compare(kind), false) => emit_inverse_jcc(asm, label, kind),
    }
}

struct BlockLabels {
    labels: Vec<CodeLabel>,
    canonical: HashMap<BlockId, usize>,
    bound: Vec<bool>,
}

impl BlockLabels {
    fn new(
        asm: &mut CodeAssembler,
        func: &MFunction,
        assignment: &AssignmentMap,
        plan: &SsaDestructionPlan,
        block_order: &[usize],
    ) -> Self {
        let mut labels = Vec::new();
        let mut canonical = HashMap::new();

        for (position, &block_index) in block_order.iter().enumerate().rev() {
            let block = &func.blocks[block_index];
            let next = block_order
                .get(position + 1)
                .map(|&next_index| func.blocks[next_index].id);
            let canonical_index = next
                .filter(|&next| block_is_empty_fallthrough(block, next, assignment, plan))
                .and_then(|next| canonical.get(&next).copied())
                .unwrap_or_else(|| {
                    let index = labels.len();
                    labels.push(asm.create_label());
                    index
                });
            canonical.insert(block.id, canonical_index);
        }

        let bound = vec![false; labels.len()];
        Self {
            labels,
            canonical,
            bound,
        }
    }

    fn index(&self, block: BlockId) -> Result<usize, EmitError> {
        self.canonical.get(&block).copied().ok_or_else(|| {
            EmitInputError::new(
                "EMIT.BRANCH_TARGET",
                None,
                None,
                None,
                format!("branch targets missing block {block}"),
            )
            .into()
        })
    }

    fn label(&self, block: BlockId) -> Result<CodeLabel, EmitError> {
        Ok(self.labels[self.index(block)?])
    }

    fn label_mut(&mut self, index: usize) -> &mut CodeLabel {
        &mut self.labels[index]
    }

    fn bind(
        &mut self,
        asm: &mut CodeAssembler,
        block: BlockId,
        index: usize,
    ) -> Result<(), EmitError> {
        if self.bound[index] {
            return Ok(());
        }
        asm.set_label(&mut self.labels[index]).map_err(|error| {
            EmitInputError::new(
                "EMIT.BLOCK_LABEL",
                Some(block),
                None,
                None,
                format!("failed to bind native block label: {error}"),
            )
        })?;
        self.bound[index] = true;
        Ok(())
    }

    fn mark_bound(&mut self, index: usize) {
        self.bound[index] = true;
    }
}

fn instruction_emits_no_code(inst: &MInst, assignment: &AssignmentMap) -> bool {
    match inst {
        MInst::X86Simd(_) => false,
        MInst::Mov { dst, src } => {
            matches!((assignment.get(*dst), assignment.get(*src)), (Some(dst), Some(src)) if dst == src)
        }
        MInst::AndImm {
            dst,
            src,
            imm: u64::MAX,
        }
        | MInst::OrImm { dst, src, imm: 0 } => {
            matches!((assignment.get(*dst), assignment.get(*src)), (Some(dst), Some(src)) if dst == src)
        }
        MInst::CmpSelect {
            dst,
            true_val,
            false_val,
            ..
        }
        | MInst::CmpImmSelect {
            dst,
            true_val,
            false_val,
            ..
        } => matches!(
            (
                assignment.get(*dst),
                assignment.get(*true_val),
                assignment.get(*false_val),
            ),
            (Some(dst), Some(true_val), Some(false_val))
                if dst == true_val && dst == false_val
        ),
        MInst::GuardedCmpSelect {
            dst,
            guard,
            lhs,
            rhs,
            true_val,
            false_val,
            ..
        } => matches!(
            (
                assignment.get(*dst),
                assignment.get(*guard),
                assignment.get(*lhs),
                assignment.get(*rhs),
                assignment.get(*true_val),
                assignment.get(*false_val),
            ),
            (Some(dst), Some(guard), Some(lhs), Some(rhs), Some(true_val), Some(false_val))
                if dst != guard
                    && dst != lhs
                    && dst != rhs
                    && dst == true_val
                    && dst == false_val
        ),
        MInst::MemCopy { byte_len: 0, .. } | MInst::MemFill { byte_len: 0, .. } => true,
        MInst::Scratch { .. }
        | MInst::SparseCommit {
            summary_word_count: 0,
            ..
        }
        | MInst::SparseCommitWorklist {
            active_capacity: 0, ..
        } => true,
        _ => false,
    }
}

fn block_is_empty_fallthrough(
    block: &MBlock,
    next: BlockId,
    assignment: &AssignmentMap,
    plan: &SsaDestructionPlan,
) -> bool {
    matches!(block.terminator(), Some(MInst::Jump { target }) if *target == next)
        && !plan
            .edge(block.id, next)
            .is_some_and(|edge| edge.has_effective_copies())
        && block.insts[..block.insts.len() - 1]
            .iter()
            .all(|inst| instruction_emits_no_code(inst, assignment))
}

/// Choose physical block order after allocation without changing MIR or its
/// SSA edge identities. RPO deliberately places a backedge-only successor
/// late: DFS finishes that edge before walking the loop exit and reversing
/// postorder moves it behind the complete exit region. That is useful for
/// forward allocation, but disastrous when a dedicated CSSA/spill edge block
/// executes on every loop iteration.
///
/// Pull only a linear, single-predecessor, phi-free chain which eventually
/// jumps to a block dominating the branch predecessor. Every selected chain
/// is disjoint because its first and subsequent blocks each have exactly one
/// predecessor. The layout walk itself is `O(B + E)` after the shared forward
/// CFG analysis; that analysis additionally owns its dominance-frontier and
/// natural-loop membership costs. No instruction-sized or pairwise value
/// structure is built here.
fn emission_block_order(func: &MFunction) -> Vec<usize> {
    let identity = (0..func.blocks.len()).collect::<Vec<_>>();
    if func.blocks.len() < 2 {
        return identity;
    }

    let block_index = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let successors = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .filter_map(|successor| block_index.get(&successor).copied())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let Ok(cfg) = ForwardControlFlowGraph::analyze(successors, 0) else {
        // Input verification reports malformed or unreachable CFGs. Layout is
        // only an optimization, so retain the supplied order on raw inputs.
        return identity;
    };

    fn backedge_chain(
        func: &MFunction,
        block_index: &HashMap<BlockId, usize>,
        cfg: &ForwardControlFlowGraph,
        predecessor: usize,
        successor: BlockId,
    ) -> Option<Vec<usize>> {
        let mut expected_predecessor = predecessor;
        let mut current = *block_index.get(&successor)?;
        let mut chain = Vec::new();

        while chain.len() < func.blocks.len() {
            // Only pull blocks currently after the latch. This preserves the
            // existing forward layout and makes every chosen chain unplaced
            // when the latch is visited during the final linear walk.
            if current <= predecessor
                || cfg.predecessors.get(current)?.as_slice() != [expected_predecessor]
                || !func.blocks[current].phis.is_empty()
            {
                return None;
            }
            let MInst::Jump { target } = func.blocks[current].terminator()? else {
                return None;
            };
            let target = *block_index.get(target)?;
            chain.push(current);
            if cfg.dominators.dominates(target, predecessor) {
                return Some(chain);
            }
            expected_predecessor = current;
            current = target;
        }
        None
    }

    let mut claimed = vec![false; func.blocks.len()];
    let mut after = vec![Vec::<usize>::new(); func.blocks.len()];
    for (predecessor, block) in func.blocks.iter().enumerate() {
        let Some((true_bb, false_bb)) = block.terminator().and_then(MInst::branch_targets) else {
            continue;
        };
        for successor in [true_bb, false_bb] {
            let Some(chain) = backedge_chain(func, &block_index, &cfg, predecessor, successor)
            else {
                continue;
            };
            if chain.iter().any(|&index| claimed[index]) {
                continue;
            }
            for &index in &chain {
                claimed[index] = true;
            }
            after[predecessor] = chain;
            break;
        }
    }

    let mut placed = vec![false; func.blocks.len()];
    let mut order = Vec::with_capacity(func.blocks.len());
    for block in 0..func.blocks.len() {
        if placed[block] {
            continue;
        }
        placed[block] = true;
        order.push(block);
        for &edge_block in &after[block] {
            debug_assert!(!placed[edge_block]);
            placed[edge_block] = true;
            order.push(edge_block);
        }
    }
    debug_assert_eq!(order.len(), func.blocks.len());
    order
}

fn branch_label(labels: &BlockLabels, block: BlockId) -> Result<CodeLabel, EmitError> {
    labels.label(block).map_err(|_| {
        EmitInputError::new(
            "EMIT.BRANCH_TARGET",
            None,
            None,
            None,
            format!("branch targets missing block {block}"),
        )
        .into()
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_branch_with_edge_copies(
    asm: &mut CodeAssembler,
    labels: &BlockLabels,
    plan: &SsaDestructionPlan,
    predecessor: BlockId,
    true_block: BlockId,
    false_block: BlockId,
    next_block: Option<BlockId>,
    condition: EmittedBranchCondition,
) -> Result<(), EmitError> {
    let true_edge = plan
        .edge(predecessor, true_block)
        .filter(|edge| edge.has_effective_copies());
    let false_edge = plan
        .edge(predecessor, false_block)
        .filter(|edge| edge.has_effective_copies());
    let true_label = branch_label(labels, true_block)?;
    let false_label = branch_label(labels, false_block)?;

    match (true_edge, false_edge) {
        (None, None) => {
            if next_block == Some(true_block) {
                // Invert the branch so the physical true successor is a real
                // fallthrough instead of a taken jump to the next instruction
                // followed by an unconditional false-edge jump.
                emit_condition_jump(asm, false_label, condition, false)?;
            } else {
                emit_condition_jump(asm, true_label, condition, true)?;
            }
            if next_block != Some(false_block) && next_block != Some(true_block) {
                asm.jmp(false_label)?;
            }
        }
        (Some(true_edge), None) => {
            // The false edge can jump directly to its target.  The true edge
            // falls through its copy sequence, avoiding an extra local stub.
            emit_condition_jump(asm, false_label, condition, false)?;
            emit_parallel_copy_plan(asm, Some(true_edge))?;
            if next_block != Some(true_block) {
                asm.jmp(true_label)?;
            }
        }
        (None, Some(false_edge)) => {
            emit_condition_jump(asm, true_label, condition, true)?;
            emit_parallel_copy_plan(asm, Some(false_edge))?;
            if next_block != Some(false_block) {
                asm.jmp(false_label)?;
            }
        }
        (Some(true_edge), Some(false_edge)) if next_block == Some(false_block) => {
            // Place the layout-successor copy last so it can fall through.
            let mut false_copy_label = asm.create_label();
            emit_condition_jump(asm, false_copy_label, condition, false)?;
            emit_parallel_copy_plan(asm, Some(true_edge))?;
            asm.jmp(true_label)?;
            asm.set_label(&mut false_copy_label)?;
            emit_parallel_copy_plan(asm, Some(false_edge))?;
        }
        (Some(true_edge), Some(false_edge)) => {
            let mut true_copy_label = asm.create_label();
            emit_condition_jump(asm, true_copy_label, condition, true)?;
            emit_parallel_copy_plan(asm, Some(false_edge))?;
            asm.jmp(false_label)?;
            asm.set_label(&mut true_copy_label)?;
            emit_parallel_copy_plan(asm, Some(true_edge))?;
            if next_block != Some(true_block) {
                asm.jmp(true_label)?;
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
) -> Result<EmitResult, EmitError> {
    verify_emission_inputs(func, assignment, spill_frame_size, false)?;
    let plan = SsaDestructionPlan::build(func, assignment)?;
    plan.verify(func, assignment, spill_frame_size)?;
    emit_planned(
        func,
        assignment,
        spill_frame_size,
        inferred_standalone_state_size(func),
        &plan,
        false,
        false,
    )
}

/// Direct emitter tests do not carry a complete `MemoryLayout`. Place their
/// native arena beyond every statically named SimState byte plus a guard so
/// test-owned sentinel bytes cannot alias prologue/scratch storage.
fn inferred_standalone_state_size(func: &MFunction) -> usize {
    let static_end = func
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .flat_map(|inst| {
            [
                super::memory_effect::reads(inst),
                super::memory_effect::writes(inst),
            ]
        })
        .flat_map(|effects| effects.ranges().collect::<Vec<_>>())
        .filter(|range| range.base == BaseReg::SimState && range.offset >= 0)
        .filter_map(|range| usize::try_from(range.end()?).ok())
        .max()
        .unwrap_or(0);
    static_end.saturating_add(4096)
}

/// Emit using the allocation phase's explicit SSA destruction artifact.
/// Verification is intentionally repeated immediately before encoding so a
/// stale or accidentally modified plan cannot reach the emitter.
pub(crate) fn emit_with_plan(
    func: &MFunction,
    assignment: &AssignmentMap,
    spill_frame_size: u32,
    state_size: usize,
    plan: &SsaDestructionPlan,
) -> Result<EmitResult, EmitError> {
    verify_emission_inputs(func, assignment, spill_frame_size, false)?;
    plan.verify(func, assignment, spill_frame_size)?;
    emit_planned(
        func,
        assignment,
        spill_frame_size,
        state_size,
        plan,
        false,
        false,
    )
}

fn emit_with_plan_tick_loop(
    func: &MFunction,
    assignment: &AssignmentMap,
    spill_frame_size: u32,
    state_size: usize,
    plan: &SsaDestructionPlan,
    check_runtime_events: bool,
) -> Result<EmitResult, EmitError> {
    verify_emission_inputs(func, assignment, spill_frame_size, true)?;
    plan.verify(func, assignment, spill_frame_size)?;
    emit_planned(
        func,
        assignment,
        spill_frame_size,
        state_size,
        plan,
        true,
        check_runtime_events,
    )
}

fn verify_emission_inputs(
    func: &MFunction,
    assignment: &AssignmentMap,
    spill_frame_size: u32,
    tick_loop: bool,
) -> Result<(), EmitError> {
    func.verify_result().map_err(EmitError::Mir)?;

    let vector_entries = assignment.sorted_x86_vectors();
    let has_vector_spill = vector_entries
        .iter()
        .any(|(_, location)| matches!(location, X86VectorLocation::Stack(_)));
    let register_limit = if tick_loop { 15 } else { 9 };
    for (value, location) in vector_entries {
        match location {
            X86VectorLocation::Register(register)
                if register.0 < register_limit && !(has_vector_spill && register.0 == 5) => {}
            X86VectorLocation::Stack(offset)
                if offset >= 0
                    && offset % 16 == 0
                    && u32::try_from(offset)
                        .ok()
                        .and_then(|offset| offset.checked_add(16))
                        .is_some_and(|end| end <= spill_frame_size) => {}
            _ => {
                return Err(EmitInputError::new(
                    "EMIT.X86_VECTOR_LOCATION",
                    None,
                    None,
                    None,
                    format!(
                        "{value} has invalid location {location:?} in {spill_frame_size}-byte spill frame"
                    ),
                )
                .into());
            }
        }
    }

    for block in &func.blocks {
        for (instruction, inst) in block.insts.iter().enumerate() {
            if let Some(value) = inst.def()
                && assignment.get(value).is_none()
            {
                return Err(EmitInputError::new(
                    "EMIT.ASSIGNMENT_COMPLETE",
                    Some(block.id),
                    Some(instruction),
                    Some(value),
                    "instruction definition has no physical register assignment",
                )
                .into());
            }
            for value in inst.uses() {
                if assignment.get(value).is_none() {
                    return Err(EmitInputError::new(
                        "EMIT.ASSIGNMENT_COMPLETE",
                        Some(block.id),
                        Some(instruction),
                        Some(value),
                        "instruction operand has no physical register assignment",
                    )
                    .into());
                }
            }
            match inst {
                MInst::Load {
                    base: BaseReg::StackFrame,
                    offset,
                    size,
                    ..
                }
                | MInst::Store {
                    base: BaseReg::StackFrame,
                    offset,
                    size,
                    ..
                } => verify_stack_frame_access(
                    block.id,
                    instruction,
                    *offset,
                    *size,
                    spill_frame_size,
                )?,
                MInst::LoadIndexed {
                    base: BaseReg::StackFrame,
                    ..
                }
                | MInst::StoreIndexed {
                    base: BaseReg::StackFrame,
                    ..
                }
                | MInst::OrStoreIndexed {
                    base: BaseReg::StackFrame,
                    ..
                } => {
                    return Err(EmitInputError::new(
                        "EMIT.STACK_FRAME_INDEXED",
                        Some(block.id),
                        Some(instruction),
                        None,
                        "indexed stack-frame access has no statically provable frame bound",
                    )
                    .into());
                }
                _ => {}
            }
        }
    }

    if spill_frame_size > (i32::MAX as u32).saturating_sub(15) {
        return Err(EmitInputError::new(
            "EMIT.FRAME_SIZE_RANGE",
            None,
            None,
            None,
            "aligned spill frame exceeds signed 32-bit x86 displacement",
        )
        .into());
    }
    Ok(())
}

fn verify_stack_frame_access(
    block: BlockId,
    instruction: usize,
    offset: i32,
    size: OpSize,
    spill_frame_size: u32,
) -> Result<(), EmitError> {
    let bytes = size.bytes();
    let valid = offset >= 0
        && u32::try_from(offset)
            .ok()
            .filter(|offset| offset % bytes == 0)
            .and_then(|offset| offset.checked_add(bytes))
            .is_some_and(|end| end <= spill_frame_size);
    if valid {
        return Ok(());
    }
    Err(EmitInputError::new(
        "EMIT.STACK_FRAME_ACCESS",
        Some(block),
        Some(instruction),
        None,
        format!(
            "{}-byte stack access at offset {offset} is not naturally aligned inside {spill_frame_size} bytes",
            bytes
        ),
    )
    .into())
}

fn emit_planned(
    func: &MFunction,
    assignment: &AssignmentMap,
    spill_frame_size: u32,
    state_size: usize,
    plan: &SsaDestructionPlan,
    tick_loop: bool,
    check_runtime_events: bool,
) -> Result<EmitResult, EmitError> {
    let mut asm = CodeAssembler::new(64)?;
    let block_order = emission_block_order(func);

    // Empty layout fallthrough chains share the label of the next block that
    // emits code. iced permits only one label on an instruction, so distinct
    // BlockIds at the same machine-code IP must be aliases here rather than
    // zero-length pseudo instructions in the assembler stream.
    let mut block_labels = BlockLabels::new(&mut asm, func, assignment, plan, &block_order);
    let mut constant_table_labels = func
        .constant_tables()
        .iter()
        .map(|_| asm.create_label())
        .collect::<Vec<_>>();
    let mut jump_table_labels = HashMap::<(BlockId, usize), CodeLabel>::new();
    for block in &func.blocks {
        for (instruction, inst) in block.insts.iter().enumerate() {
            if matches!(inst, MInst::JumpTable { .. }) {
                jump_table_labels.insert((block.id, instruction), asm.create_label());
            }
        }
    }

    let state_base = func.target_features.state_base();
    let arena = NativeArenaLayout::build(
        func,
        assignment,
        state_size,
        spill_frame_size,
        state_base,
        tick_loop,
    )?;
    debug_assert!(arena.scratch_size >= 4 * 8);
    ACTIVE_SPILL_BASE.with(|base| base.set(arena.spill_base));
    ACTIVE_SCRATCH_BASE.with(|base| base.set(arena.scratch_base));
    ACTIVE_STATE_BASE.with(|active| active.set(state_base));

    let mut epilogue_label = asm.create_label();
    let mut tick_loop_success_label = tick_loop.then(|| asm.create_label());
    let use_counts = count_vreg_uses(func, plan);
    let spill_register_cache = select_spill_register_cache(func, plan, assignment, tick_loop);
    let tick_loop_entry = tick_loop
        .then(|| branch_label(&block_labels, func.blocks[0].id))
        .transpose()?;

    // ── Prologue ──
    {
        if let Some(save_offset) = arena.loop_xmm15_save {
            asm.movdqu(xmmword_ptr(rdi + save_offset), xmm15)?;
        }
        // The GPR allocator does not own vector registers. Preserve the used
        // callee-saved GPRs outside that file. GS mode additionally preserves
        // the caller's segment base; fallback mode borrows the saved R15.
        match (tick_loop, state_base) {
            (true, StateBaseStrategy::Fs) => {
                asm.rdfsbase(rax)?;
                asm.mov(
                    qword_ptr(rdi + arena.loop_segment_save.expect("loop segment save")),
                    rax,
                )?;
            }
            (true, StateBaseStrategy::Gs) => {
                asm.rdgsbase(rax)?;
                asm.mov(
                    qword_ptr(rdi + arena.loop_segment_save.expect("loop segment save")),
                    rax,
                )?;
            }
            (true, StateBaseStrategy::R15) => {}
            (false, StateBaseStrategy::Fs) => {
                asm.rdfsbase(rax)?;
                asm.movq(xmm15, rax)?;
            }
            (false, StateBaseStrategy::Gs) => {
                asm.rdgsbase(rax)?;
                asm.movq(xmm15, rax)?;
            }
            (false, StateBaseStrategy::R15) => {}
        }
        if let Some(base) = arena.loop_gpr_save_base {
            for (index, &reg) in arena.callee_saved.iter().enumerate() {
                asm.mov(
                    qword_ptr(rdi + base + i32::try_from(index * 8).unwrap()),
                    preg_to_reg64(reg),
                )?;
            }
        } else {
            for (index, &reg) in arena.callee_saved.iter().enumerate() {
                asm.movq(saved_gpr_xmm(index), preg_to_reg64(reg))?;
            }
        }
        match state_base {
            StateBaseStrategy::Fs => asm.wrfsbase(rdi)?,
            StateBaseStrategy::Gs => asm.wrgsbase(rdi)?,
            StateBaseStrategy::R15 => asm.mov(r15, rdi)?,
        }
        if tick_loop {
            let mut count_ready = asm.create_label();
            asm.mov(
                rax,
                qword_ptr(mem_operand(
                    BaseReg::SimState,
                    STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET as i32,
                )),
            )?;
            asm.test(rax, rax)?;
            asm.jne(count_ready)?;
            asm.mov(eax, 1u32)?;
            asm.set_label(&mut count_ready)?;
            // XMM15 is invisible to GPR allocation. Its low qword carries the
            // tick count.
            asm.movq(xmm15, rax)?;
            // Keep the internal count-ready label distinct from the first MIR
            // block label when runtime-event initialization is absent.
            asm.nop()?;

            if check_runtime_events {
                asm.mov(
                    rax,
                    qword_ptr(mem_operand(
                        BaseReg::SimState,
                        STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET as i32,
                    )),
                )?;
                asm.mov(rax, qword_ptr(rax))?;
                asm.mov(
                    qword_ptr(mem_operand(
                        BaseReg::SimState,
                        STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET as i32,
                    )),
                    rax,
                )?;
            }
        }
    }

    // ── Blocks ──
    let mut previous_canonical_label = None;
    for (order_idx, &bi) in block_order.iter().enumerate() {
        let block = &func.blocks[bi];
        let next_block_id = block_order
            .get(order_idx + 1)
            .map(|&next_bi| func.blocks[next_bi].id);

        let canonical_label = block_labels.index(block.id)?;
        if previous_canonical_label != Some(canonical_label) {
            block_labels.bind(&mut asm, block.id, canonical_label)?;
        }
        previous_canonical_label = Some(canonical_label);

        let fallthrough_continuation = block.insts[..block.insts.len() - 1]
            .iter()
            .rposition(|inst| !instruction_emits_no_code(inst, assignment))
            .zip(next_block_id.filter(|&next| {
                matches!(block.terminator(), Some(MInst::Jump { target }) if *target == next)
                    && !plan
                        .edge(block.id, next)
                        .is_some_and(|edge| edge.has_effective_copies())
            }))
            .map(|(instruction, next)| block_labels.index(next).map(|label| (instruction, label)))
            .transpose()?;

        let mut inst_idx = 0usize;
        while inst_idx < block.insts.len() {
            let inst = &block.insts[inst_idx];
            match inst {
                MInst::Return => {
                    if tick_loop {
                        asm.movq(rax, xmm15)?;
                        asm.dec(rax)?;
                        asm.movq(xmm15, rax)?;
                        asm.jz(tick_loop_success_label.expect("tick-loop success label"))?;
                        if check_runtime_events {
                            asm.mov(
                                rax,
                                qword_ptr(mem_operand(
                                    BaseReg::SimState,
                                    STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET as i32,
                                )),
                            )?;
                            asm.mov(rax, qword_ptr(rax))?;
                            asm.cmp(
                                rax,
                                qword_ptr(mem_operand(
                                    BaseReg::SimState,
                                    STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET as i32,
                                )),
                            )?;
                            asm.jne(tick_loop_success_label.expect("tick-loop success label"))?;
                        }
                        asm.jmp(tick_loop_entry.expect("tick-loop entry label"))?;
                    } else {
                        asm.xor(eax, eax)?;
                        asm.jmp(epilogue_label)?;
                    }
                }
                MInst::ReturnError { code } => {
                    if tick_loop {
                        asm.movq(rax, xmm15)?;
                        asm.dec(rax)?;
                        asm.movq(xmm15, rax)?;
                    }
                    asm.mov(eax, *code as u32)?;
                    asm.jmp(epilogue_label)?;
                }
                MInst::Jump { target } => {
                    let edge = plan
                        .edge(block.id, *target)
                        .filter(|edge| edge.has_effective_copies());
                    emit_parallel_copy_plan(&mut asm, edge)?;
                    if next_block_id != Some(*target) {
                        asm.jmp(branch_label(&block_labels, *target)?)?;
                    }
                }
                MInst::Branch {
                    cond,
                    true_bb,
                    false_bb,
                } => {
                    let c = preg_to_reg64(resolve(assignment, *cond));
                    asm.test(c, c)?;
                    emit_branch_with_edge_copies(
                        &mut asm,
                        &block_labels,
                        plan,
                        block.id,
                        *true_bb,
                        *false_bb,
                        next_block_id,
                        EmittedBranchCondition::NonZero,
                    )?;
                }
                MInst::BranchPred {
                    predicate,
                    true_bb,
                    false_bb,
                } => {
                    let condition = emit_branch_predicate(&mut asm, *predicate, assignment)?;
                    emit_branch_with_edge_copies(
                        &mut asm,
                        &block_labels,
                        plan,
                        block.id,
                        *true_bb,
                        *false_bb,
                        next_block_id,
                        condition,
                    )?;
                }
                MInst::JumpTable {
                    index,
                    table_base,
                    target,
                    ..
                } => {
                    let index = preg_to_reg64(resolve(assignment, *index));
                    let table_base = preg_to_reg64(resolve(assignment, *table_base));
                    let target = preg_to_reg64(resolve(assignment, *target));
                    let label = jump_table_labels[&(block.id, inst_idx)];
                    asm.lea(table_base, ptr(label))?;
                    asm.movsxd(target, dword_ptr(table_base + index * 4))?;
                    asm.add(target, table_base)?;
                    asm.jmp(target)?;
                }
                MInst::UDiv { dst, lhs, rhs } => {
                    emit_divrem(&mut asm, assignment, *dst, *lhs, *rhs, DivOp::Div)?;
                }
                MInst::URem { dst, lhs, rhs } => {
                    emit_divrem(&mut asm, assignment, *dst, *lhs, *rhs, DivOp::Rem)?;
                }
                MInst::SDiv { dst, lhs, rhs } => {
                    emit_divrem(&mut asm, assignment, *dst, *lhs, *rhs, DivOp::SDiv)?;
                }
                MInst::SRem { dst, lhs, rhs } => {
                    emit_divrem(&mut asm, assignment, *dst, *lhs, *rhs, DivOp::SRem)?;
                }
                _ => {
                    if inst_idx + 1 < block.insts.len()
                        && try_emit_stack_reload_fold(
                            &mut asm,
                            inst,
                            &block.insts[inst_idx + 1],
                            &use_counts,
                            assignment,
                            func,
                            spill_register_cache,
                        )?
                    {
                        inst_idx += 2;
                        continue;
                    }
                    let continuation_label = fallthrough_continuation
                        .filter(|(instruction, _)| *instruction == inst_idx)
                        .map(|(_, label)| label);
                    let bound_continuation = if let Some(index) = continuation_label {
                        emit_inst(
                            &mut asm,
                            inst,
                            assignment,
                            func,
                            &constant_table_labels,
                            spill_register_cache,
                            Some(block_labels.label_mut(index)),
                        )?
                    } else {
                        emit_inst(
                            &mut asm,
                            inst,
                            assignment,
                            func,
                            &constant_table_labels,
                            spill_register_cache,
                            None,
                        )?
                    };
                    if let (true, Some(index)) = (bound_continuation, continuation_label) {
                        block_labels.mark_bound(index);
                    }
                }
            }
            inst_idx += 1;
        }
    }

    // ── Epilogue ──
    if let Some(label) = &mut tick_loop_success_label {
        asm.set_label(label)?;
        asm.xor(eax, eax)?;
    }
    asm.set_label(&mut epilogue_label)?;
    if tick_loop {
        asm.movq(r10, xmm15)?;
        asm.mov(
            qword_ptr(mem_operand(
                BaseReg::SimState,
                STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET as i32,
            )),
            r10,
        )?;
    }
    if let Some(save_offset) = arena.loop_xmm15_save {
        asm.movdqu(
            xmm15,
            xmmword_ptr(mem_operand(BaseReg::SimState, save_offset)),
        )?;
    }
    if let Some(base) = arena.loop_gpr_save_base {
        for (index, &reg) in arena.callee_saved.iter().enumerate() {
            asm.mov(
                preg_to_reg64(reg),
                qword_ptr(mem_operand(
                    BaseReg::SimState,
                    base + i32::try_from(index * 8).unwrap(),
                )),
            )?;
        }
    } else {
        for (index, &reg) in arena.callee_saved.iter().enumerate().rev() {
            asm.movq(preg_to_reg64(reg), saved_gpr_xmm(index))?;
        }
    }
    match (tick_loop, state_base) {
        (true, StateBaseStrategy::Fs) => {
            asm.mov(
                r11,
                qword_ptr(mem_operand(
                    BaseReg::SimState,
                    arena.loop_segment_save.expect("loop segment save"),
                )),
            )?;
            asm.wrfsbase(r11)?;
        }
        (true, StateBaseStrategy::Gs) => {
            asm.mov(
                r11,
                qword_ptr(mem_operand(
                    BaseReg::SimState,
                    arena.loop_segment_save.expect("loop segment save"),
                )),
            )?;
            asm.wrgsbase(r11)?;
        }
        (true, StateBaseStrategy::R15) => {}
        (false, StateBaseStrategy::Fs) => {
            asm.movq(r11, xmm15)?;
            asm.wrfsbase(r11)?;
        }
        (false, StateBaseStrategy::Gs) => {
            asm.movq(r11, xmm15)?;
            asm.wrgsbase(r11)?;
        }
        (false, StateBaseStrategy::R15) => {}
    }
    asm.ret()?;

    // Keep immutable lookup and dispatch data out of every control-flow path.
    // Jump tables contain signed offsets from their own table base, preserving
    // relocatability when the complete code image is copied into JIT memory.
    for block in &func.blocks {
        for (instruction, inst) in block.insts.iter().enumerate() {
            let MInst::JumpTable { targets, .. } = inst else {
                continue;
            };
            let label = jump_table_labels
                .get_mut(&(block.id, instruction))
                .expect("every jump table has an assembler label");
            asm.set_label(label)?;
            asm.dd(&vec![0u32; targets.len()])?;
        }
    }

    // Keep immutable lookup data out of every control-flow path. Table
    // addresses are encoded RIP-relatively, so the resulting code remains
    // relocatable when copied into executable memory by the JIT.
    for (label, table) in constant_table_labels.iter_mut().zip(func.constant_tables()) {
        asm.set_label(label)?;
        asm.dq(table)?;
    }

    let mut result =
        asm.assemble_options(0x0, BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS)?;
    let first_data_label = jump_table_labels
        .values()
        .chain(constant_table_labels.iter())
        .min_by_key(|label| result.label_ip(label).unwrap_or(u64::MAX));
    let text_size = if let Some(label) = first_data_label {
        usize::try_from(result.label_ip(label).map_err(|error| {
            EmitInputError::new(
                "EMIT.CONSTANT_TABLE_LABEL_IP",
                None,
                None,
                None,
                format!("failed to resolve native constant-table label: {error}"),
            )
        })?)
        .map_err(|_| {
            EmitInputError::new(
                "EMIT.CONSTANT_TABLE_LABEL_IP",
                None,
                None,
                None,
                "native text size exceeds usize",
            )
        })?
    } else {
        result.inner.code_buffer.len()
    };
    let mut block_offsets = Vec::with_capacity(func.blocks.len());
    for block in &func.blocks {
        let label = block_labels.label(block.id)?;
        let ip = result.label_ip(&label).map_err(|error| {
            EmitInputError::new(
                "EMIT.BLOCK_LABEL_IP",
                Some(block.id),
                None,
                None,
                format!("failed to resolve native block label: {error}"),
            )
        })?;
        block_offsets.push((block.id, ip));
    }
    let block_ips = block_offsets.iter().copied().collect::<HashMap<_, _>>();
    for block in &func.blocks {
        for (instruction, inst) in block.insts.iter().enumerate() {
            let MInst::JumpTable { targets, .. } = inst else {
                continue;
            };
            let label = &jump_table_labels[&(block.id, instruction)];
            let table_ip = result.label_ip(label).map_err(|error| {
                EmitInputError::new(
                    "EMIT.JUMP_TABLE_LABEL_IP",
                    Some(block.id),
                    Some(instruction),
                    None,
                    format!("failed to resolve jump-table label: {error}"),
                )
            })?;
            let table_offset = usize::try_from(table_ip).map_err(|_| {
                EmitInputError::new(
                    "EMIT.JUMP_TABLE_OFFSET",
                    Some(block.id),
                    Some(instruction),
                    None,
                    "jump-table offset exceeds usize",
                )
            })?;
            for (index, target) in targets.iter().enumerate() {
                let target_ip = block_ips[target];
                let relative = i64::try_from(target_ip)
                    .expect("assembler block offset fits i64")
                    .checked_sub(i64::try_from(table_ip).expect("assembler table offset fits i64"))
                    .and_then(|offset| i32::try_from(offset).ok())
                    .ok_or_else(|| {
                        EmitInputError::new(
                            "EMIT.JUMP_TABLE_TARGET_RANGE",
                            Some(block.id),
                            Some(instruction),
                            None,
                            format!("jump-table target {target} exceeds signed 32-bit reach"),
                        )
                    })?;
                let entry = table_offset + index * 4;
                result.inner.code_buffer[entry..entry + 4].copy_from_slice(&relative.to_le_bytes());
            }
        }
    }
    Ok(EmitResult {
        code: result.inner.code_buffer,
        text_size,
        frame_size: spill_frame_size,
        required_state_size: arena.total_size,
        block_offsets,
    })
}

fn count_vreg_uses(func: &MFunction, plan: &SsaDestructionPlan) -> HashMap<VReg, usize> {
    let mut counts = HashMap::new();
    for edge in plan.edges() {
        for row in &edge.rows {
            *counts.entry(row.source_value).or_default() += 1;
        }
    }
    for block in &func.blocks {
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
    spill_register_cache: SpillRegisterCache,
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
    if spill_register_cache.register(*offset).is_some() {
        return Ok(false);
    }
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
    _func: &MFunction,
) -> Result<bool, IcedError> {
    match inst {
        MInst::Mov { dst, src } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.mov(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            Ok(true)
        }
        MInst::Mov32 { dst, src } if *src == stack_vreg => {
            let d = preg_to_reg32(resolve(assignment, *dst));
            asm.mov(d, dword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            Ok(true)
        }
        MInst::Add { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::Add,
            false,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Add32 { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::Add,
            true,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Sub { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::Sub,
            false,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Sub32 { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::Sub,
            true,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Mul { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::Mul,
            false,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Mul32 { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::Mul,
            true,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::And { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::And,
            false,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::And32 { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::And,
            true,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Or { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::Or,
            false,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Or32 { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::Or,
            true,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Xor { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::Xor,
            false,
            *dst,
            *lhs,
            *rhs,
            stack_vreg,
            stack_offset,
        ),
        MInst::Xor32 { dst, lhs, rhs } => emit_binop_stack_mem(
            asm,
            assignment,
            BinOp::Xor,
            true,
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
        MInst::AndImm32 { dst, src, imm } if *src == stack_vreg => {
            let d = preg_to_reg32(resolve(assignment, *dst));
            asm.mov(d, dword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
            asm.and(d, *imm as i32)?;
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
        MInst::Bsf { dst, src } if *src == stack_vreg => {
            let d = preg_to_reg64(resolve(assignment, *dst));
            asm.bsf(d, qword_ptr(mem_operand(BaseReg::StackFrame, stack_offset)))?;
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
    constant_table_labels: &[CodeLabel],
    spill_register_cache: SpillRegisterCache,
    mut continuation_label: Option<&mut CodeLabel>,
) -> Result<bool, IcedError> {
    let mut bound_continuation = false;
    match inst {
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
        MInst::X86Simd(X86SimdInst::Pack128 { dst, low, high }) => {
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
                        asm.movq(xmm5, high)?;
                        asm.punpcklqdq(destination, xmm5)?;
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
            let d = preg_to_reg64(resolve(assignment, *dst));
            let s = preg_to_reg64(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            emit_and_imm64(asm, d, *imm)?;
        }
        MInst::AndImm32 { dst, src, imm } => {
            let d = preg_to_reg32(resolve(assignment, *dst));
            let s = preg_to_reg32(resolve(assignment, *src));
            if d != s {
                asm.mov(d, s)?;
            }
            asm.and(d, *imm as i32)?;
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

fn emit_inverse_jcc(
    asm: &mut CodeAssembler,
    label: CodeLabel,
    kind: CmpKind,
) -> Result<(), IcedError> {
    match kind {
        CmpKind::Eq => asm.jne(label),
        CmpKind::Ne => asm.je(label),
        CmpKind::LtU => asm.jae(label),
        CmpKind::LtS => asm.jge(label),
        CmpKind::LeU => asm.ja(label),
        CmpKind::LeS => asm.jg(label),
        CmpKind::GtU => asm.jbe(label),
        CmpKind::GtS => asm.jle(label),
        CmpKind::GeU => asm.jb(label),
        CmpKind::GeS => asm.jl(label),
    }
}

fn emit_cmovcc(
    asm: &mut CodeAssembler,
    dst: AsmRegister64,
    src: AsmRegister64,
    kind: CmpKind,
) -> Result<(), IcedError> {
    match kind {
        CmpKind::Eq => asm.cmove(dst, src),
        CmpKind::Ne => asm.cmovne(dst, src),
        CmpKind::LtU => asm.cmovb(dst, src),
        CmpKind::LtS => asm.cmovl(dst, src),
        CmpKind::LeU => asm.cmovbe(dst, src),
        CmpKind::LeS => asm.cmovle(dst, src),
        CmpKind::GtU => asm.cmova(dst, src),
        CmpKind::GtS => asm.cmovg(dst, src),
        CmpKind::GeU => asm.cmovae(dst, src),
        CmpKind::GeS => asm.cmovge(dst, src),
    }
}

fn emit_inverse_cmovcc(
    asm: &mut CodeAssembler,
    dst: AsmRegister64,
    src: AsmRegister64,
    kind: CmpKind,
) -> Result<(), IcedError> {
    match kind {
        CmpKind::Eq => asm.cmovne(dst, src),
        CmpKind::Ne => asm.cmove(dst, src),
        CmpKind::LtU => asm.cmovae(dst, src),
        CmpKind::LtS => asm.cmovge(dst, src),
        CmpKind::LeU => asm.cmova(dst, src),
        CmpKind::LeS => asm.cmovg(dst, src),
        CmpKind::GtU => asm.cmovbe(dst, src),
        CmpKind::GtS => asm.cmovle(dst, src),
        CmpKind::GeU => asm.cmovb(dst, src),
        CmpKind::GeS => asm.cmovl(dst, src),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_cmp_select(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    kind: CmpKind,
    true_val: VReg,
    false_val: VReg,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));
    let tv = preg_to_reg64(resolve(assignment, true_val));
    let fv = preg_to_reg64(resolve(assignment, false_val));

    if tv == fv {
        if d != tv {
            asm.mov(d, tv)?;
        }
        return Ok(());
    }

    asm.cmp(l, r)?;
    if d == fv {
        emit_cmovcc(asm, d, tv, kind)?;
    } else if d == tv {
        emit_inverse_cmovcc(asm, d, fv, kind)?;
    } else {
        asm.mov(d, fv)?;
        emit_cmovcc(asm, d, tv, kind)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_cmp_imm_select(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    imm: i32,
    kind: CmpKind,
    true_val: VReg,
    false_val: VReg,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let tv = preg_to_reg64(resolve(assignment, true_val));
    let fv = preg_to_reg64(resolve(assignment, false_val));

    if tv == fv {
        if d != tv {
            asm.mov(d, tv)?;
        }
        return Ok(());
    }

    if imm == 0 && matches!(kind, CmpKind::Eq | CmpKind::Ne) {
        asm.test(l, l)?;
    } else {
        asm.cmp(l, imm)?;
    }
    if d == fv {
        emit_cmovcc(asm, d, tv, kind)?;
    } else if d == tv {
        emit_inverse_cmovcc(asm, d, fv, kind)?;
    } else {
        asm.mov(d, fv)?;
        emit_cmovcc(asm, d, tv, kind)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_guarded_cmp_select(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    guard: VReg,
    lhs: VReg,
    rhs: VReg,
    kind: CmpKind,
    true_val: VReg,
    false_val: VReg,
    continuation_label: Option<&mut CodeLabel>,
) -> Result<bool, IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let g = preg_to_reg64(resolve(assignment, guard));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));
    let tv = preg_to_reg64(resolve(assignment, true_val));
    let fv = preg_to_reg64(resolve(assignment, false_val));

    if d == g || d == l || d == r {
        return emit_guarded_cmp_select_branchy(asm, d, g, l, r, kind, tv, fv, continuation_label);
    }

    if tv == fv {
        if d != tv {
            asm.mov(d, tv)?;
        }
    } else if d == fv {
        if let Some(done) = continuation_label {
            asm.test(g, g)?;
            asm.je(*done)?;
            asm.cmp(l, r)?;
            emit_cmovcc(asm, d, tv, kind)?;
            asm.set_label(done)?;
            return Ok(true);
        } else {
            let mut done = asm.create_label();
            asm.test(g, g)?;
            asm.je(done)?;
            asm.cmp(l, r)?;
            emit_cmovcc(asm, d, tv, kind)?;
            asm.set_label(&mut done)?;
        }
    } else if d == tv {
        asm.cmp(l, r)?;
        emit_inverse_cmovcc(asm, d, fv, kind)?;
        asm.test(g, g)?;
        asm.cmove(d, fv)?;
    } else {
        asm.mov(d, fv)?;
        asm.cmp(l, r)?;
        emit_cmovcc(asm, d, tv, kind)?;
        asm.test(g, g)?;
        asm.cmove(d, fv)?;
    }
    Ok(false)
}

fn emit_guarded_cmp_select_branchy(
    asm: &mut CodeAssembler,
    dst: AsmRegister64,
    guard: AsmRegister64,
    lhs: AsmRegister64,
    rhs: AsmRegister64,
    kind: CmpKind,
    true_val: AsmRegister64,
    false_val: AsmRegister64,
    continuation_label: Option<&mut CodeLabel>,
) -> Result<bool, IcedError> {
    let mut false_label = asm.create_label();
    let mut true_label = asm.create_label();
    if let Some(done) = continuation_label {
        asm.test(guard, guard)?;
        asm.je(false_label)?;
        asm.cmp(lhs, rhs)?;
        emit_jcc(asm, true_label, kind)?;
        asm.set_label(&mut false_label)?;
        if dst != false_val {
            asm.mov(dst, false_val)?;
        }
        asm.jmp(*done)?;
        asm.set_label(&mut true_label)?;
        if dst != true_val {
            asm.mov(dst, true_val)?;
        } else {
            asm.nop()?;
        }
        asm.set_label(done)?;
        Ok(true)
    } else {
        let mut done = asm.create_label();
        asm.test(guard, guard)?;
        asm.je(false_label)?;
        asm.cmp(lhs, rhs)?;
        emit_jcc(asm, true_label, kind)?;
        asm.set_label(&mut false_label)?;
        if dst != false_val {
            asm.mov(dst, false_val)?;
        }
        asm.jmp(done)?;
        asm.set_label(&mut true_label)?;
        if dst != true_val {
            asm.mov(dst, true_val)?;
        } else {
            asm.nop()?;
        }
        asm.set_label(&mut done)?;
        Ok(false)
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

/// Emit the shift encoding selected by the function's target-feature snapshot.
fn emit_shift(
    asm: &mut CodeAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: ShiftOp,
    encoding: VariableShiftEncoding,
) -> Result<(), IcedError> {
    let d = preg_to_reg64(resolve(assignment, dst));
    let l = preg_to_reg64(resolve(assignment, lhs));
    let r = preg_to_reg64(resolve(assignment, rhs));

    match encoding {
        VariableShiftEncoding::Bmi2 => match op {
            ShiftOp::Shr => asm.shrx(d, l, r)?,
            ShiftOp::Shl => asm.shlx(d, l, r)?,
            ShiftOp::Sar => asm.sarx(d, l, r)?,
        },
        VariableShiftEncoding::LegacyCl => {
            // The allocation verifier proves the fixed-use constraint.
            debug_assert!(r == rcx, "legacy shift rhs must be in RCX");
            if d == rcx && l != rcx {
                // Moving lhs into RCX first would destroy the count in CL.
                // Shift an arena-saved copy in place and reload it into RCX, so
                // the original lhs register remains untouched.
                asm.mov(qword_ptr(scratch_operand(0)), l)?;
                match op {
                    ShiftOp::Shr => asm.shr(qword_ptr(scratch_operand(0)), cl)?,
                    ShiftOp::Shl => asm.shl(qword_ptr(scratch_operand(0)), cl)?,
                    ShiftOp::Sar => asm.sar(qword_ptr(scratch_operand(0)), cl)?,
                }
                asm.mov(rcx, qword_ptr(scratch_operand(0)))?;
            } else {
                if d != l {
                    asm.mov(d, l)?;
                }
                match op {
                    ShiftOp::Shr => asm.shr(d, cl)?,
                    ShiftOp::Shl => asm.shl(d, cl)?,
                    ShiftOp::Sar => asm.sar(d, cl)?,
                }
            }
        }
    }
    Ok(())
}

/// Division operation kind.
#[derive(Clone, Copy)]
enum DivOp {
    Div, // quotient in RAX
    Rem, // remainder in RDX
    SDiv,
    SRem,
}

/// Emit integer division/remainder using unsigned `div` or signed `idiv`.
/// Both consume RDX:RAX and produce the quotient in RAX and remainder in RDX.
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
        DivOp::Div | DivOp::SDiv => rax,
        DivOp::Rem | DivOp::SRem => rdx,
    };
    let signed = matches!(op, DivOp::SDiv | DivOp::SRem);

    // Divisor cannot be read from RAX/RDX because div consumes RDX:RAX.
    // Use a stack copy instead of an unmodeled scratch register clobber.
    let rhs_on_stack = r == rax || r == rdx;
    if rhs_on_stack {
        asm.mov(qword_ptr(scratch_operand(0)), r)?;
    }

    if l != rax {
        asm.mov(rax, l)?;
    }
    if signed {
        asm.cqo()?;
    } else {
        asm.xor(edx, edx)?;
    }
    if rhs_on_stack {
        if signed {
            asm.idiv(qword_ptr(scratch_operand(0)))?;
        } else {
            asm.div(qword_ptr(scratch_operand(0)))?;
        }
    } else if signed {
        asm.idiv(r)?;
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
    op: BinOp,
    narrow32: bool,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    stack_vreg: VReg,
    stack_offset: i32,
) -> Result<bool, IcedError> {
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
            _ if d == r15 => r15d,
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

/// Lower one fully prepared SIR function through x86 ISel, MIR optimization,
/// register allocation, and emission.
///
/// SIR merging and backend-boundary SIR optimization happen before this call;
/// the x86 crate therefore consumes an immutable backend-independent artifact.
pub fn emit_prepared_eu(
    sir_eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    layout: &crate::MemoryLayout,
    four_state: bool,
    label: &str,
    options: &crate::X86BackendOptions,
    mut trace: Option<&mut NativeFunctionTrace>,
) -> Result<EmitResult, ChainedEmitError> {
    use super::{isel, regalloc};
    let diagnostics = &options.diagnostics;
    let timing = diagnostics.phase_timing;
    let mir_stats = diagnostics.mir_stats;
    let copy_stats =
        timing || mir_stats || diagnostics.regalloc_timing || diagnostics.regalloc_stats;
    let total_start = timing.then(crate::timing::now);

    sir_eu
        .verify_result()
        .map_err(|error| ChainedEmitError::Sir {
            phase: "at x86 backend boundary",
            error,
        })?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.optimized_sir = sir_eu.to_string();
    }
    if timing {
        log_sir_width_stats(sir_eu);
    }

    // Single ISel + optimize + regalloc + emit
    let isel_start = timing.then(crate::timing::now);
    let mut mfunc =
        isel::lower_execution_unit_with_diagnostics(sir_eu, layout, four_state, diagnostics);
    if let Some(start) = isel_start {
        tracing::debug!(
            "[native-timing] emit_chained isel mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    dump_native_block_context(label, "after_isel", sir_eu, &mfunc, diagnostics);
    if timing {
        tracing::debug!("[native-timing] emit_chained verify after_isel label={label}");
    }
    mfunc
        .verify_result()
        .map_err(|error| ChainedEmitError::Mir {
            phase: "after native instruction selection",
            error,
        })?;
    let legalize_start = timing.then(crate::timing::now);
    super::mir_legalize::legalize(&mut mfunc);
    if let Some(start) = legalize_start {
        tracing::debug!(
            "[native-timing] emit_chained legalize mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    dump_native_block_context(label, "after_legalize", sir_eu, &mfunc, diagnostics);
    if timing {
        tracing::debug!("[native-timing] emit_chained verify after_legalize label={label}");
    }
    mfunc
        .verify_result()
        .map_err(|error| ChainedEmitError::Mir {
            phase: "after MIR legalization",
            error,
        })?;
    let opt_start = timing.then(crate::timing::now);
    super::mir_opt::optimize_with_diagnostics(&mut mfunc, diagnostics);
    if let Some(start) = opt_start {
        tracing::debug!(
            "[native-timing] emit_chained mir_opt label={label} mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    mfunc
        .verify_result()
        .map_err(|error| ChainedEmitError::Mir {
            phase: "after MIR optimization before x86 SLP",
            error,
        })?;
    let slp_stats = if options.slp {
        super::x86_slp::select(&mut mfunc)
    } else {
        super::x86_slp::SlpStats::default()
    };
    mfunc
        .verify_result()
        .map_err(|error| ChainedEmitError::Mir {
            phase: "after x86 SLP before VReg compaction",
            error,
        })?;
    if timing {
        tracing::debug!(
            "[native-timing] emit_chained x86_slp vector_zeroes={} vector_packs={} vector_loads={} vector_binary_ops={} vector_stores={} scalar_instructions_removed={}",
            slp_stats.vector_zeroes,
            slp_stats.vector_packs,
            slp_stats.vector_loads,
            slp_stats.vector_binary_ops,
            slp_stats.vector_stores,
            slp_stats.scalar_instructions_removed,
        );
    }
    let compact_start = timing.then(crate::timing::now);
    let compacted = super::mir_opt::compact_vregs(&mut mfunc);
    if let Some(start) = compact_start {
        tracing::debug!(
            "[native-timing] emit_chained compact_vregs before={} after={} removed={} elapsed={:?}",
            compacted.before,
            compacted.after,
            compacted.before - compacted.after,
            start.elapsed()
        );
    }
    if mir_stats {
        log_mir_stats(label, "after_mir_opt", &mfunc);
    }
    if diagnostics.mir_block_stats {
        log_mir_block_stats(label, "after_mir_opt", &mfunc);
    }
    dump_native_block_context(label, "after_mir_opt", sir_eu, &mfunc, diagnostics);
    if timing {
        tracing::debug!("[native-timing] emit_chained verify after_mir_opt label={label}");
    }
    mfunc
        .verify_result()
        .map_err(|error| ChainedEmitError::Mir {
            phase: "after MIR optimization",
            error,
        })?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.mir_before_regalloc = mfunc.to_string();
    }
    let regalloc_start = timing.then(crate::timing::now);
    let mut regalloc_trace = trace.as_ref().map(|_| regalloc::RegallocTrace::default());
    let ra = regalloc::run_regalloc_with_label_and_trace_and_diagnostics(
        &mut mfunc,
        label,
        regalloc_trace.as_mut(),
        diagnostics,
        options.native_tick_loop,
    )?;
    if let (Some(trace), Some(regalloc_trace)) = (trace.as_deref_mut(), regalloc_trace.as_mut()) {
        trace.mir_after_late_memory_folds =
            std::mem::take(&mut regalloc_trace.mir_after_late_memory_folds);
        trace.mir_after_scheduling = std::mem::take(&mut regalloc_trace.mir_after_scheduling);
    }
    if let Some(start) = regalloc_start {
        tracing::debug!(
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
    super::mir_opt::post_regalloc_cleanup(&mut mfunc);
    super::mir_opt::post_regalloc_direct_load_cse(&mut mfunc, &ra.assignment);
    regalloc::verify_assignment(&mfunc, &ra.assignment)?;
    if let Some(start) = post_regalloc_start {
        tracing::debug!(
            "[native-timing] emit_chained post_regalloc_cleanup mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    mfunc
        .verify_result()
        .map_err(|error| ChainedEmitError::Mir {
            phase: "after post-allocation MIR peepholes",
            error,
        })?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.mir_after_regalloc = mfunc.to_string();
        trace.register_assignment.clear();
        for (vreg, preg) in ra.assignment.sorted_entries() {
            trace
                .register_assignment
                .push_str(&format!("  {vreg} -> {preg}\n"));
        }
        trace.spill_frame_size = ra.spill_frame_size;
    }
    if mir_stats {
        log_mir_stats(label, "after_regalloc", &mfunc);
    }
    if diagnostics.mir_block_stats {
        log_mir_block_stats(label, "after_regalloc", &mfunc);
    }
    dump_native_block_context(label, "after_regalloc", sir_eu, &mfunc, diagnostics);
    // Post-allocation peepholes and CFG cleanup can change the physical value
    // present on a phi edge. Build the edge-copy plan from this final MIR, not
    // from the pre-cleanup allocation input.
    let ssa_destruction = SsaDestructionPlan::build(&mfunc, &ra.assignment)?;
    ssa_destruction.verify(&mfunc, &ra.assignment, ra.spill_frame_size)?;
    if copy_stats {
        let stats = ssa_destruction.stats();
        tracing::debug!(
            "[native-edge-copy-stats] label={label} edges={} rows={} identity_rows={} effective_copies={} identity_only_edges={} direct_moves={} register_swaps={} cycle_breaks={} temporary_cycle_breaks={} ready_pops={} dependency_releases={} max_effective_per_edge={}",
            stats.edges,
            stats.rows,
            stats.identity_rows,
            stats.effective_copies,
            stats.identity_only_edges,
            stats.direct_moves,
            stats.register_swaps,
            stats.cycle_breaks,
            stats.temporary_cycle_breaks,
            stats.ready_queue_pops,
            stats.dependency_releases,
            stats.max_effective_copies_per_edge,
        );
    }
    let emit_start = timing.then(crate::timing::now);
    let state_size = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .expect("native simulation-state size overflow");
    let result = if label == "eval_comb_apply_ff" && options.native_tick_loop {
        let check_runtime_events = sir_eu.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    crate::SIRInstruction::RuntimeEvent { .. }
                        | crate::SIRInstruction::CombCaptureEvent { .. }
                )
            })
        });
        emit_with_plan_tick_loop(
            &mfunc,
            &ra.assignment,
            ra.spill_frame_size,
            state_size,
            &ssa_destruction,
            check_runtime_events,
        )?
    } else {
        emit_with_plan(
            &mfunc,
            &ra.assignment,
            ra.spill_frame_size,
            state_size,
            &ssa_destruction,
        )?
    };
    if let Some(trace) = trace {
        trace.disassembly = disassemble_with_block_offsets(
            &result.code[..result.text_size],
            0,
            &result.block_offsets,
        );
    }
    if let Some(start) = emit_start {
        tracing::debug!(
            "[native-timing] emit_chained emit bytes={} elapsed={:?}",
            result.code.len(),
            start.elapsed()
        );
    }
    if let Some(start) = total_start {
        tracing::debug!(
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
                MInst::X86Simd(X86SimdInst::Zero128 { .. })
                | MInst::X86Simd(X86SimdInst::Pack128 { .. })
                | MInst::X86Simd(X86SimdInst::Binary128 { .. }) => alu += 1,
                MInst::X86Simd(X86SimdInst::Load128 { base, .. }) => match base {
                    BaseReg::SimState => load_sim += 1,
                    BaseReg::StackFrame => load_stack += 1,
                },
                MInst::X86Simd(X86SimdInst::Store128 { base, .. }) => match base {
                    BaseReg::SimState => store_sim += 1,
                    BaseReg::StackFrame => store_stack += 1,
                },
                MInst::Mov { .. } | MInst::Mov32 { .. } => mov += 1,
                MInst::LoadImm { .. } | MInst::LoadConstantTableAddr { .. } => imm += 1,
                MInst::Scratch { .. } => {}
                MInst::Load { base, .. } => match base {
                    BaseReg::SimState => load_sim += 1,
                    BaseReg::StackFrame => load_stack += 1,
                },
                MInst::Store { base, .. } => match base {
                    BaseReg::SimState => store_sim += 1,
                    BaseReg::StackFrame => store_stack += 1,
                },
                MInst::AndStoreImm { base, .. } | MInst::OrStoreImm { base, .. } => match base {
                    BaseReg::SimState => {
                        load_sim += 1;
                        store_sim += 1;
                    }
                    BaseReg::StackFrame => {
                        load_stack += 1;
                        store_stack += 1;
                    }
                },
                MInst::LoadPtr { .. } => load_ptr += 1,
                MInst::StorePtr { .. } | MInst::ReleaseStorePtr { .. } => store_ptr += 1,
                MInst::LoadIndexed { .. }
                | MInst::LoadPtrIndexed { .. }
                | MInst::PackedLaneCompare { .. } => indexed_load += 1,
                MInst::StoreIndexed { .. }
                | MInst::OrStoreIndexed { .. }
                | MInst::StorePtrIndexed { .. }
                | MInst::ReleaseStorePtrIndexed { .. } => indexed_store += 1,
                MInst::MemCopy { .. }
                | MInst::MemFill { .. }
                | MInst::SparseCommit { .. }
                | MInst::SparseMarkActive { .. }
                | MInst::SparseCommitWorklist { .. } => memcopy += 1,
                MInst::Add { .. }
                | MInst::Add32 { .. }
                | MInst::Sub { .. }
                | MInst::Sub32 { .. }
                | MInst::Mul { .. }
                | MInst::Mul32 { .. }
                | MInst::UMulHi { .. }
                | MInst::And { .. }
                | MInst::And32 { .. }
                | MInst::Or { .. }
                | MInst::Or32 { .. }
                | MInst::Xor { .. }
                | MInst::Xor32 { .. }
                | MInst::Shr { .. }
                | MInst::Shl { .. }
                | MInst::Sar { .. } => alu += 1,
                MInst::AndImm { .. }
                | MInst::AndImm32 { .. }
                | MInst::OrImm { .. }
                | MInst::ShrImm { .. }
                | MInst::ShlImm { .. }
                | MInst::SarImm { .. }
                | MInst::AddImm { .. }
                | MInst::SubImm { .. } => alu_imm += 1,
                MInst::Cmp { .. }
                | MInst::CmpImm { .. }
                | MInst::PackedByteAffineCompare { .. } => cmp += 1,
                MInst::UDiv { .. }
                | MInst::URem { .. }
                | MInst::SDiv { .. }
                | MInst::SRem { .. } => div_rem += 1,
                MInst::BitNot { .. }
                | MInst::Neg { .. }
                | MInst::Popcnt { .. }
                | MInst::Bsf { .. }
                | MInst::Bsr { .. }
                | MInst::BsrOr { .. }
                | MInst::Pext { .. }
                | MInst::Pdep { .. } => bit_ops += 1,
                MInst::Select { .. }
                | MInst::CmpSelect { .. }
                | MInst::CmpImmSelect { .. }
                | MInst::GuardedCmpSelect { .. } => select += 1,
                MInst::Branch { .. } => branch += 1,
                MInst::BranchPred { predicate, .. } => {
                    branch += 1;
                    match predicate {
                        BranchPredicate::Compare { .. } | BranchPredicate::CompareImm { .. } => {
                            cmp += 1;
                        }
                        BranchPredicate::MemoryNonZero { base, .. } => match base {
                            BaseReg::SimState => load_sim += 1,
                            BaseReg::StackFrame => load_stack += 1,
                        },
                    }
                }
                MInst::JumpTable { .. } => jump += 1,
                MInst::Jump { .. } => jump += 1,
                MInst::Return | MInst::ReturnError { .. } => ret += 1,
            }
        }
    }

    tracing::debug!(
        "[native-mir-stats] label={label} stage={stage} phi={phi} mov={mov} imm={imm} load_sim={load_sim} load_stack={load_stack} load_ptr={load_ptr} store_sim={store_sim} store_stack={store_stack} store_ptr={store_ptr} indexed_load={indexed_load} indexed_store={indexed_store} memcopy={memcopy} alu={alu} alu_imm={alu_imm} cmp={cmp} div_rem={div_rem} bit_ops={bit_ops} select={select} branch={branch} jump={jump} ret={ret}"
    );
}

fn log_mir_block_stats(label: &str, stage: &str, func: &super::mir::MFunction) {
    let mut blocks = func
        .blocks
        .iter()
        .map(|block| {
            let insts = block.phis.len() + block.insts.len();
            let mut load_sim = 0usize;
            let mut load_stack = 0usize;
            let mut store_sim = 0usize;
            let mut store_stack = 0usize;
            let mut indexed_mem = 0usize;
            let mut memcopy = 0usize;
            let mut imm = 0usize;
            let mut alu = 0usize;
            let mut alu_imm = 0usize;
            let mut cmp = 0usize;
            let mut bit_ops = 0usize;
            let mut select = 0usize;
            let mut control = 0usize;
            for inst in &block.insts {
                match inst {
                    MInst::Load { base, .. } => match base {
                        BaseReg::SimState => load_sim += 1,
                        BaseReg::StackFrame => load_stack += 1,
                    },
                    MInst::Store { base, .. } => match base {
                        BaseReg::SimState => store_sim += 1,
                        BaseReg::StackFrame => store_stack += 1,
                    },
                    MInst::LoadIndexed { .. }
                    | MInst::LoadPtrIndexed { .. }
                    | MInst::StoreIndexed { .. }
                    | MInst::OrStoreIndexed { .. }
                    | MInst::StorePtrIndexed { .. }
                    | MInst::ReleaseStorePtrIndexed { .. } => indexed_mem += 1,
                    MInst::MemCopy { .. } | MInst::MemFill { .. } => memcopy += 1,
                    MInst::LoadImm { .. } | MInst::LoadConstantTableAddr { .. } => imm += 1,
                    MInst::Add { .. }
                    | MInst::Add32 { .. }
                    | MInst::Sub { .. }
                    | MInst::Sub32 { .. }
                    | MInst::Mul { .. }
                    | MInst::Mul32 { .. }
                    | MInst::UMulHi { .. }
                    | MInst::And { .. }
                    | MInst::And32 { .. }
                    | MInst::Or { .. }
                    | MInst::Or32 { .. }
                    | MInst::Xor { .. }
                    | MInst::Xor32 { .. }
                    | MInst::Shr { .. }
                    | MInst::Shl { .. }
                    | MInst::Sar { .. } => alu += 1,
                    MInst::AndImm { .. }
                    | MInst::AndImm32 { .. }
                    | MInst::OrImm { .. }
                    | MInst::ShrImm { .. }
                    | MInst::ShlImm { .. }
                    | MInst::SarImm { .. }
                    | MInst::AddImm { .. }
                    | MInst::SubImm { .. } => alu_imm += 1,
                    MInst::Cmp { .. } | MInst::CmpImm { .. } => cmp += 1,
                    MInst::BitNot { .. }
                    | MInst::Neg { .. }
                    | MInst::Popcnt { .. }
                    | MInst::Bsf { .. }
                    | MInst::Bsr { .. }
                    | MInst::BsrOr { .. }
                    | MInst::Pext { .. }
                    | MInst::Pdep { .. } => bit_ops += 1,
                    MInst::Select { .. }
                    | MInst::CmpSelect { .. }
                    | MInst::CmpImmSelect { .. }
                    | MInst::GuardedCmpSelect { .. } => select += 1,
                    MInst::Branch { .. }
                    | MInst::BranchPred { .. }
                    | MInst::Jump { .. }
                    | MInst::Return
                    | MInst::ReturnError { .. } => control += 1,
                    _ => {}
                }
            }
            (
                insts,
                block.id.0,
                block.phis.len(),
                block.insts.len(),
                load_sim,
                load_stack,
                store_sim,
                store_stack,
                indexed_mem,
                memcopy,
                imm,
                alu,
                alu_imm,
                cmp,
                bit_ops,
                select,
                control,
            )
        })
        .collect::<Vec<_>>();
    blocks.sort_unstable_by_key(|entry| (std::cmp::Reverse(entry.0), entry.1));
    for (
        rank,
        (
            total,
            block_id,
            phis,
            insts,
            load_sim,
            load_stack,
            store_sim,
            store_stack,
            indexed_mem,
            memcopy,
            imm,
            alu,
            alu_imm,
            cmp,
            bit_ops,
            select,
            control,
        ),
    ) in blocks.into_iter().take(10).enumerate()
    {
        tracing::debug!(
            "[native-mir-block-stats] label={label} stage={stage} rank={} block={} total={} phis={} insts={} load_sim={} load_stack={} store_sim={} store_stack={} indexed_mem={} memcopy={} imm={} alu={} alu_imm={} cmp={} bit_ops={} select={} control={}",
            rank + 1,
            block_id,
            total,
            phis,
            insts,
            load_sim,
            load_stack,
            store_sim,
            store_stack,
            indexed_mem,
            memcopy,
            imm,
            alu,
            alu_imm,
            cmp,
            bit_ops,
            select,
            control
        );
    }
}

fn dump_native_block_context(
    label: &str,
    stage: &str,
    eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    func: &super::mir::MFunction,
    diagnostics: &crate::NativeDiagnostics,
) {
    let Some(options) = diagnostics.dump.as_ref() else {
        return;
    };
    if let Some(configured_label) = options.label.as_deref()
        && configured_label != label
    {
        return;
    }
    if let Some(configured_stage) = options.stage.as_deref() {
        if configured_stage != stage {
            return;
        }
    } else if stage != "after_isel" {
        return;
    }
    let Ok(block_id) = u32::try_from(options.block) else {
        return;
    };
    let sir_id = crate::BlockId(block_id as usize);
    tracing::debug!("[native-dump] label={label} stage={stage} block={block_id}");
    if options.dump_sir {
        if let Some(block) = eu.blocks.get(&sir_id) {
            tracing::debug!("[native-dump] SIR:\n{block}");
            dump_sir_operand_defs(eu, block);
        } else {
            tracing::debug!("[native-dump] SIR block b{block_id} not found");
        }
    }
    if let Some(block) = func
        .blocks
        .iter()
        .find(|block| block.id == super::mir::BlockId(block_id))
    {
        tracing::debug!(
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
            tracing::debug!("  {} = phi({sources})", phi.dst);
        }
        for (idx, inst) in block.insts.iter().enumerate().take(options.mir_limit) {
            tracing::debug!("  {idx}: {inst}");
        }
        if block.insts.len() > options.mir_limit {
            tracing::debug!("  ... {} more insts", block.insts.len() - options.mir_limit);
        }
    } else {
        tracing::debug!("[native-dump] MIR block b{block_id} not found");
    }
}

fn dump_sir_operand_defs(
    eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    block: &crate::BasicBlock<crate::RegionedAbsoluteAddr>,
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
                tracing::debug!("  [sir-def] r{} is param of b{}", reg.0, other.id.0);
                found = true;
            }
            for (idx, inst) in other.instructions.iter().enumerate() {
                if sir_inst_def(inst) == Some(reg) {
                    tracing::debug!(
                        "  [sir-def] r{} defined at b{} inst {}: {}",
                        reg.0,
                        other.id.0,
                        idx,
                        inst
                    );
                    found = true;
                }
            }
        }
        if !found {
            tracing::debug!("  [sir-def] r{} has no SIR definition", reg.0);
        }
    }
}

fn sir_inst_def(
    inst: &crate::SIRInstruction<crate::RegionedAbsoluteAddr>,
) -> Option<crate::RegisterId> {
    use crate::SIRInstruction;
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
    inst: &crate::SIRInstruction<crate::RegionedAbsoluteAddr>,
    out: &mut Vec<crate::RegisterId>,
) {
    use crate::SIRInstruction;
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

fn log_sir_width_stats(eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>) {
    use crate::{RegisterType, SIRInstruction};

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
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable();
    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
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

    tracing::debug!(
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
        tracing::debug!("[native-timing] sir_width_example {example}");
    }
}

#[cfg(test)]
mod shift_encoding_tests {
    use super::*;
    use crate::native::features::{StateBaseStrategy, X86Features};
    use crate::native::jit_mem::JitCode;
    use crate::native::{mir_legalize, mir_opt, regalloc};
    use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, Register};

    #[test]
    fn vector_binary_ops_execute_both_lanes_without_gpr_roundtrips() {
        for op in [
            X86SimdBinaryOp::And,
            X86SimdBinaryOp::Or,
            X86SimdBinaryOp::Xor,
        ] {
            let mut function = MFunction::new(VRegAllocator::new(), Vec::new());
            let lhs = function.alloc_x86_vec();
            let rhs = function.alloc_x86_vec();
            let result = function.alloc_x86_vec();
            let mut block = MBlock::new(BlockId(0));
            block.push(MInst::X86Simd(X86SimdInst::Load128 {
                dst: lhs,
                base: BaseReg::SimState,
                offset: 0,
            }));
            block.push(MInst::X86Simd(X86SimdInst::Load128 {
                dst: rhs,
                base: BaseReg::SimState,
                offset: 16,
            }));
            block.push(MInst::X86Simd(X86SimdInst::Binary128 {
                op,
                dst: result,
                lhs,
                rhs,
            }));
            block.push(MInst::X86Simd(X86SimdInst::Store128 {
                base: BaseReg::SimState,
                offset: 32,
                src: result,
            }));
            block.push(MInst::Return);
            function.push_block(block);
            function.verify();

            let mut assignment = AssignmentMap::default();
            assignment.set_x86_vector(lhs, X86VectorLocation::Register(X86PhysVec(0)));
            assignment.set_x86_vector(rhs, X86VectorLocation::Register(X86PhysVec(1)));
            assignment.set_x86_vector(result, X86VectorLocation::Register(X86PhysVec(2)));
            let emitted = emit(&function, &assignment, 0).unwrap();
            let jit = JitCode::new(&emitted.code).unwrap();
            let lhs_lanes: [u64; 2] = [0xf0f0_aaaa_5555_0f0f, 0x0123_4567_89ab_cdef];
            let rhs_lanes: [u64; 2] = [0x3333_ffff_0000_cccc, 0xfedc_ba98_7654_3210];
            let mut state = [0u8; 48];
            for (lane, value) in lhs_lanes.into_iter().enumerate() {
                state[lane * 8..lane * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
            for (lane, value) in rhs_lanes.into_iter().enumerate() {
                let offset = 16 + lane * 8;
                state[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            }

            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            for lane in 0..2 {
                let offset = 32 + lane * 8;
                let actual = u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap());
                let expected = match op {
                    X86SimdBinaryOp::And => lhs_lanes[lane] & rhs_lanes[lane],
                    X86SimdBinaryOp::Or => lhs_lanes[lane] | rhs_lanes[lane],
                    X86SimdBinaryOp::Xor => lhs_lanes[lane] ^ rhs_lanes[lane],
                };
                assert_eq!(actual, expected, "{op:?}");
            }
        }
    }

    #[test]
    fn native_tick_loop_reenters_the_allocated_body_without_reentering_the_abi_boundary() {
        let mut vregs = VRegAllocator::new();
        let current = vregs.alloc();
        let one = vregs.alloc();
        let next = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: current,
            base: BaseReg::SimState,
            offset: 64,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm { dst: one, value: 1 });
        block.push(MInst::Add {
            dst: next,
            lhs: current,
            rhs: one,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 64,
            src: next,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        function.push_block(block);

        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        let plan = SsaDestructionPlan::build(&function, &allocation.assignment).unwrap();
        let emitted = emit_planned(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
            4096,
            &plan,
            true,
            false,
        )
        .unwrap();
        let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
        while decoder.can_decode() {
            let instruction = decoder.decode();
            assert_ne!(
                instruction.memory_base(),
                Register::RSP,
                "native tick loop must not address through RSP: {instruction}"
            );
            for operand in 0..instruction.op_count() {
                assert_ne!(
                    instruction.op_register(operand),
                    Register::RSP,
                    "native tick loop must not borrow RSP: {instruction}"
                );
            }
        }
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut memory = vec![0u64; emitted.required_state_size as usize / 8 + 1];
        let event_sequence = 0u64;
        memory[STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET / 8] = (&event_sequence as *const u64) as u64;
        memory[STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET / 8] = 5;

        let result = unsafe { (jit.fn_ptr)(memory.as_mut_ptr().cast()) };

        assert_eq!(result, 0);
        assert_eq!(memory[64 / 8], 5);
        assert_eq!(memory[STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET / 8], 0);
    }

    #[test]
    fn traced_disassembly_labels_exact_basic_block_offsets() {
        let text = disassemble_with_block_offsets(
            &[0x90, 0xc3],
            0x1000,
            &[(BlockId(9), 1), (BlockId(3), 0)],
        );

        assert_eq!(text, "bb3:\n  0x00001000  nop\nbb9:\n  0x00001001  ret\n");
        assert_eq!(
            disassemble(&[0x90, 0xc3], 0x1000),
            "  0x00001000  nop\n  0x00001001  ret\n"
        );
    }

    #[test]
    fn repeated_qword_spill_accesses_use_an_unallocated_xmm_register() {
        let mut vregs = VRegAllocator::new();
        let seed = vregs.alloc();
        let first_load = vregs.alloc();
        let first_increment = vregs.alloc();
        let second_load = vregs.alloc();
        let second_increment = vregs.alloc();
        let final_load = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: seed,
            value: 7,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 0,
            src: seed,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: first_load,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::AddImm {
            dst: first_increment,
            src: first_load,
            imm: 1,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 0,
            src: first_increment,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: second_load,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::AddImm {
            dst: second_increment,
            src: second_load,
            imm: 1,
        });
        block.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 0,
            src: second_increment,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: final_load,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: final_load,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.blocks.push(block);
        func.verify();

        let mut assignment = AssignmentMap::default();
        for (value, register) in [
            (seed, PhysReg::RAX),
            (first_load, PhysReg::RBX),
            (first_increment, PhysReg::RCX),
            (second_load, PhysReg::RDX),
            (second_increment, PhysReg::RSI),
            (final_load, PhysReg::R8),
        ] {
            assignment.set(value, register);
        }

        let emitted = emit(&func, &assignment, 8).unwrap();
        let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
        let mut uses_xmm_cache = false;
        while decoder.can_decode() {
            let instruction = decoder.decode();
            uses_xmm_cache |= [instruction.op0_register(), instruction.op1_register()]
                .into_iter()
                .any(|register| {
                    (Register::XMM0 as u32..=Register::XMM14 as u32).contains(&(register as u32))
                });
        }
        assert!(uses_xmm_cache);

        let jit = JitCode::new(&emitted.code).unwrap();
        let mut arena = [0u8; 128];
        assert_eq!(unsafe { jit.call(&mut arena) }, 0);
        assert_eq!(u64::from_le_bytes(arena[..8].try_into().unwrap()), 9);
    }

    #[test]
    fn native_tick_loop_uses_all_nonconflicting_xmm_spill_cache_registers() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut block = MBlock::new(BlockId(0));
        for offset in (0..9).map(|slot| slot * 8) {
            for _ in 0..2 {
                block.push(MInst::Load {
                    dst: value,
                    base: BaseReg::StackFrame,
                    offset,
                    size: OpSize::S64,
                });
                block.push(MInst::Store {
                    base: BaseReg::StackFrame,
                    offset,
                    src: value,
                    size: OpSize::S64,
                });
            }
        }
        block.push(MInst::Return);
        function.push_block(block);
        let plan = SsaDestructionPlan::default();
        let assignment = AssignmentMap::default();

        let standalone = select_spill_register_cache(&function, &plan, &assignment, false);
        let tick_loop = select_spill_register_cache(&function, &plan, &assignment, true);

        for offset in (0..9).map(|slot| slot * 8) {
            assert!(standalone.register(offset).is_some());
        }
        for offset in (0..9).map(|slot| slot * 8) {
            assert!(tick_loop.register(offset).is_some());
        }
    }

    #[test]
    fn emission_layout_pulls_a_backedge_chain_next_to_its_latch() {
        let mut vregs = VRegAllocator::new();
        let outer_condition = vregs.alloc();
        let loop_condition = vregs.alloc();
        let edge_value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.push(MInst::Branch {
            cond: outer_condition,
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut latch = MBlock::new(BlockId(2));
        latch.push(MInst::Branch {
            cond: loop_condition,
            true_bb: BlockId(4),
            false_bb: BlockId(3),
        });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Return);
        let mut first_edge = MBlock::new(BlockId(4));
        first_edge.push(MInst::Jump { target: BlockId(5) });
        let mut second_edge = MBlock::new(BlockId(5));
        second_edge.push(MInst::Mov {
            dst: edge_value,
            src: outer_condition,
        });
        second_edge.push(MInst::Jump { target: BlockId(1) });
        func.blocks = vec![entry, header, latch, exit, first_edge, second_edge];

        let order = emission_block_order(&func)
            .into_iter()
            .map(|index| func.blocks[index].id)
            .collect::<Vec<_>>();

        assert_eq!(
            order,
            vec![
                BlockId(0),
                BlockId(1),
                BlockId(2),
                BlockId(4),
                BlockId(5),
                BlockId(3),
            ]
        );
    }

    #[test]
    fn branch_inverts_when_true_successor_is_the_physical_fallthrough() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut true_block = MBlock::new(BlockId(1));
        true_block.push(MInst::Return);
        let mut false_block = MBlock::new(BlockId(2));
        false_block.push(MInst::Return);
        func.blocks = vec![entry, true_block, false_block];

        let mut assignment = AssignmentMap::default();
        assignment.set(condition, PhysReg::RAX);
        let emitted = emit(&func, &assignment, 0).unwrap();
        let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
        let mut conditional_branches = Vec::new();
        while decoder.can_decode() {
            let instruction = decoder.decode();
            if matches!(instruction.mnemonic(), Mnemonic::Je | Mnemonic::Jne) {
                conditional_branches.push(instruction.mnemonic());
            }
        }

        assert_eq!(conditional_branches, vec![Mnemonic::Je]);
    }

    #[test]
    fn dense_jump_table_executes_every_masked_index() {
        let mut vregs = VRegAllocator::new();
        let loaded = vregs.alloc();
        let index = vregs.alloc();
        let table_base = vregs.alloc();
        let target = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: loaded,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S8,
        });
        entry.push(MInst::AndImm32 {
            dst: index,
            src: loaded,
            imm: 3,
        });
        entry.push(MInst::Scratch { dst: table_base });
        entry.push(MInst::Scratch { dst: target });
        entry.push(MInst::JumpTable {
            index,
            table_base,
            target,
            targets: (1..=4).map(BlockId).collect(),
        });
        func.blocks.push(entry);
        for code in 1..=4 {
            let mut arm = MBlock::new(BlockId(code));
            arm.push(MInst::ReturnError {
                code: i64::from(code),
            });
            func.blocks.push(arm);
        }
        func.verify();

        let mut assignment = AssignmentMap::default();
        assignment.set(loaded, PhysReg::RAX);
        assignment.set(index, PhysReg::RCX);
        assignment.set(table_base, PhysReg::RDX);
        assignment.set(target, PhysReg::RBX);
        let emitted = emit(&func, &assignment, 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        for index in 0..4u8 {
            let mut state = [0xfcu8 | index];
            assert_eq!(unsafe { jit.call(&mut state) }, i64::from(index) + 1);
        }
    }

    #[test]
    fn memory_branch_predicate_compares_in_place_for_every_width() {
        for size in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64] {
            let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
            let mut entry = MBlock::new(BlockId(0));
            entry.push(MInst::BranchPred {
                predicate: BranchPredicate::MemoryNonZero {
                    base: BaseReg::SimState,
                    offset: 8,
                    size,
                },
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            });
            let mut true_block = MBlock::new(BlockId(1));
            true_block.push(MInst::ReturnError { code: 7 });
            let mut false_block = MBlock::new(BlockId(2));
            false_block.push(MInst::ReturnError { code: 9 });
            func.blocks = vec![entry, true_block, false_block];
            func.verify();

            let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
            let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
            let mut mnemonics = Vec::new();
            while decoder.can_decode() {
                mnemonics.push(decoder.decode().mnemonic());
            }
            assert!(
                mnemonics.contains(&Mnemonic::Cmp),
                "{size:?}: {mnemonics:?}"
            );
            assert!(
                !mnemonics.contains(&Mnemonic::Movzx),
                "{size:?}: {mnemonics:?}"
            );

            let jit = JitCode::new(&emitted.code).unwrap();
            let mut state = [0u8; 16];
            assert_eq!(unsafe { jit.call(&mut state) }, 9);
            state[8] = 1;
            assert_eq!(unsafe { jit.call(&mut state) }, 7);
        }
    }

    fn decode_shift(
        op: ShiftOp,
        encoding: VariableShiftEncoding,
        dst: PhysReg,
        lhs: PhysReg,
        rhs: PhysReg,
    ) -> Vec<Instruction> {
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), dst);
        assignment.set(VReg(1), lhs);
        assignment.set(VReg(2), rhs);
        let mut asm = CodeAssembler::new(64).unwrap();
        emit_shift(
            &mut asm,
            &assignment,
            VReg(0),
            VReg(1),
            VReg(2),
            op,
            encoding,
        )
        .unwrap();
        let code = asm.assemble(0).unwrap();
        let mut decoder = Decoder::new(64, &code, DecoderOptions::NONE);
        let mut instructions = Vec::new();
        while decoder.can_decode() {
            instructions.push(decoder.decode());
        }
        instructions
    }

    #[test]
    fn and_u32_immediate_supports_r15() {
        let mut asm = CodeAssembler::new(64).unwrap();
        emit_and_imm64(&mut asm, r15, u32::MAX as u64).unwrap();
        let code = asm.assemble(0).unwrap();
        let mut decoder = Decoder::new(64, &code, DecoderOptions::NONE);
        let instruction = decoder.decode();

        assert_eq!(instruction.mnemonic(), Mnemonic::And);
        assert_eq!(instruction.op0_register(), Register::R15D);
        assert!(!decoder.can_decode());
    }

    #[test]
    fn bmi2_shifts_use_three_arbitrary_register_operands() {
        for (op, mnemonic) in [
            (ShiftOp::Shr, Mnemonic::Shrx),
            (ShiftOp::Shl, Mnemonic::Shlx),
            (ShiftOp::Sar, Mnemonic::Sarx),
        ] {
            let instructions = decode_shift(
                op,
                VariableShiftEncoding::Bmi2,
                PhysReg::R8,
                PhysReg::R9,
                PhysReg::R10,
            );
            assert_eq!(instructions.len(), 1, "{instructions:?}");
            assert_eq!(instructions[0].mnemonic(), mnemonic);
            assert_eq!(instructions[0].op0_register(), Register::R8);
            assert_eq!(instructions[0].op1_register(), Register::R9);
            assert_eq!(instructions[0].op2_register(), Register::R10);
        }
    }

    #[test]
    fn legacy_shift_uses_cl_after_copying_the_lhs() {
        let instructions = decode_shift(
            ShiftOp::Shl,
            VariableShiftEncoding::LegacyCl,
            PhysReg::R8,
            PhysReg::R9,
            PhysReg::RCX,
        );

        assert_eq!(
            instructions
                .iter()
                .map(Instruction::mnemonic)
                .collect::<Vec<_>>(),
            vec![Mnemonic::Mov, Mnemonic::Shl]
        );
        assert_eq!(instructions[1].op0_register(), Register::R8);
        assert_eq!(instructions[1].op1_register(), Register::CL);
    }

    #[test]
    fn legacy_shift_with_rcx_destination_uses_an_r15_arena_copy() {
        let instructions = decode_shift(
            ShiftOp::Shl,
            VariableShiftEncoding::LegacyCl,
            PhysReg::RCX,
            PhysReg::R8,
            PhysReg::RCX,
        );

        assert_eq!(
            instructions
                .iter()
                .map(Instruction::mnemonic)
                .collect::<Vec<_>>(),
            vec![Mnemonic::Mov, Mnemonic::Shl, Mnemonic::Mov]
        );
        assert_eq!(instructions[0].op1_register(), Register::R8);
        assert_eq!(instructions[1].memory_base(), Register::R15);
        assert_eq!(instructions[1].segment_prefix(), Register::None);
        assert_eq!(instructions[1].op1_register(), Register::CL);
        assert_eq!(instructions[2].op0_register(), Register::RCX);
    }

    #[test]
    fn fsgsbase_target_uses_gs_state_addressing_without_reserving_r15() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        func.target_features = X86Features::for_test_with_state_base(false, StateBaseStrategy::Gs);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: value,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let mut assignment = AssignmentMap::default();
        assignment.set(value, PhysReg::R15);

        let emitted = emit(&func, &assignment, 0).unwrap();
        let mut decoder =
            Decoder::new(64, &emitted.code[..emitted.text_size], DecoderOptions::NONE);
        let mut instructions = Vec::new();
        while decoder.can_decode() {
            instructions.push(decoder.decode());
        }

        assert!(
            instructions
                .iter()
                .any(|inst| inst.mnemonic() == Mnemonic::Rdgsbase)
        );
        assert!(instructions.iter().any(|inst| {
            inst.segment_prefix() == Register::GS && inst.memory_base() == Register::None
        }));
        assert!(
            instructions
                .iter()
                .any(|inst| inst.mnemonic() == Mnemonic::Wrgsbase)
        );
    }

    #[test]
    fn legacy_rcx_destination_executes_without_clobbering_live_lhs() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let count = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        func.target_features = X86Features::for_test(false);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 5 });
        block.push(MInst::LoadImm {
            dst: count,
            value: 3,
        });
        block.push(MInst::Shl {
            dst: result,
            lhs,
            rhs: count,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: result,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: lhs,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let mut assignment = AssignmentMap::default();
        assignment.set(lhs, PhysReg::R8);
        assignment.set(count, PhysReg::RCX);
        assignment.set(result, PhysReg::RCX);
        let emitted = emit(&func, &assignment, 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0u8; 16];

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(u64::from_le_bytes(state[0..8].try_into().unwrap()), 40);
        assert_eq!(u64::from_le_bytes(state[8..16].try_into().unwrap()), 5);
    }

    #[test]
    fn memfill_executes_qwords_and_every_tail_width_without_touching_neighbors() {
        const START: usize = 5;
        const LEN: usize = 23;

        let mut func = MFunction::new(VRegAllocator::new(), vec![]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::MemFill {
            dst_offset: START as i32,
            byte_len: LEN,
            value: 0x5a,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0xa5u8; 40];

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(&state[..START], &[0xa5; START]);
        assert_eq!(&state[START..START + LEN], &[0x5a; LEN]);
        assert_eq!(&state[START + LEN..], &[0xa5; 40 - START - LEN]);
    }

    #[test]
    fn nonoverlapping_memcopy_executes_vector_body_and_tail_without_touching_neighbors() {
        const SRC: usize = 3;
        const DST: usize = 64;
        const LEN: usize = 37;

        let mut func = MFunction::new(VRegAllocator::new(), vec![]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::MemCopy {
            src_offset: SRC as i32,
            dst_offset: DST as i32,
            byte_len: LEN,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0xa5u8; 112];
        for (index, byte) in state[SRC..SRC + LEN].iter_mut().enumerate() {
            *byte = index as u8 ^ 0x6d;
        }
        let expected = state[SRC..SRC + LEN].to_vec();

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(&state[DST..DST + LEN], expected);
        assert_eq!(state[DST - 1], 0xa5);
        assert_eq!(state[DST + LEN], 0xa5);
    }

    #[test]
    fn overlapping_memcopy_executes_backward_without_corrupting_source() {
        const SRC: usize = 3;
        const DST: usize = 8;
        const LEN: usize = 37;

        let mut func = MFunction::new(VRegAllocator::new(), vec![]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::MemCopy {
            src_offset: SRC as i32,
            dst_offset: DST as i32,
            byte_len: LEN,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0xa5u8; 64];
        for (index, byte) in state.iter_mut().enumerate() {
            *byte = index as u8 ^ 0x6d;
        }
        let mut expected = state;
        expected.copy_within(SRC..SRC + LEN, DST);

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(state, expected);
    }

    #[test]
    fn overlapping_memcopy_executes_forward_without_corrupting_source() {
        const SRC: usize = 8;
        const DST: usize = 3;
        const LEN: usize = 37;

        let mut func = MFunction::new(VRegAllocator::new(), vec![]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::MemCopy {
            src_offset: SRC as i32,
            dst_offset: DST as i32,
            byte_len: LEN,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0xa5u8; 64];
        for (index, byte) in state.iter_mut().enumerate() {
            *byte = index as u8 ^ 0xb3;
        }
        let mut expected = state;
        expected.copy_within(SRC..SRC + LEN, DST);

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(state, expected);
    }

    #[test]
    fn sparse_mark_active_is_register_free_and_preserves_live_values() {
        const OUTPUT: usize = 0;
        const INPUT: usize = 8;
        const ACTIVE_BITS: usize = 16;
        const ACTIVE_INDEX: u32 = 65;

        let mut vregs = VRegAllocator::new();
        let live_value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: live_value,
            base: BaseReg::SimState,
            offset: INPUT as i32,
            size: OpSize::S64,
        });
        block.push(MInst::SparseMarkActive {
            active_index: ACTIVE_INDEX,
            active_bits_offset: ACTIVE_BITS as i32,
            active_capacity: 70,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: OUTPUT as i32,
            src: live_value,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);

        mir_legalize::legalize(&mut func);
        mir_opt::optimize(&mut func);
        let allocation = regalloc::run_regalloc(&mut func).unwrap();
        let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();

        let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
        let mut bit_sets = 0;
        while decoder.can_decode() {
            let instruction = decoder.decode();
            bit_sets += usize::from(instruction.mnemonic() == Mnemonic::Bts);
            assert!(
                !matches!(instruction.mnemonic(), Mnemonic::Push | Mnemonic::Pop)
                    || instruction.op0_register() != Register::RAX,
                "SparseMarkActive emitted a hidden RAX save/restore: {instruction}"
            );
        }
        assert_eq!(bit_sets, 1);

        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0u8; 64];
        state[INPUT..INPUT + 8].copy_from_slice(&7u64.to_le_bytes());
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[OUTPUT..OUTPUT + 8].try_into().unwrap()),
            7
        );
        assert_eq!(
            u64::from_le_bytes(state[ACTIVE_BITS..ACTIVE_BITS + 8].try_into().unwrap()),
            0
        );
        assert_eq!(
            u64::from_le_bytes(state[ACTIVE_BITS + 8..ACTIVE_BITS + 16].try_into().unwrap()),
            1 << (ACTIVE_INDEX % 64)
        );
    }

    #[test]
    fn sparse_mark_active_shares_a_fallthrough_block_label() {
        const ACTIVE_BITS: usize = 8;
        const ACTIVE_INDEX: u32 = 3;

        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::SparseMarkActive {
            active_index: ACTIVE_INDEX,
            active_bits_offset: ACTIVE_BITS as i32,
            active_capacity: 4,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(1));
        exit.push(MInst::Return);
        func.push_block(entry);
        func.push_block(exit);

        mir_legalize::legalize(&mut func);
        mir_opt::optimize(&mut func);
        let allocation = regalloc::run_regalloc(&mut func).unwrap();
        let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0u8; 64];

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[ACTIVE_BITS..ACTIVE_BITS + 8].try_into().unwrap()),
            1 << ACTIVE_INDEX
        );
    }

    #[test]
    fn sparse_commit_clobbers_preserve_live_values_without_hidden_pushes() {
        const STABLE: usize = 0;
        const SPARSE: usize = 16;
        const DIRTY: usize = 32;
        const SUMMARY: usize = 40;
        const OLD_OUTPUT: usize = 48;

        let mut vregs = VRegAllocator::new();
        let old_stable = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: old_stable,
            base: BaseReg::SimState,
            offset: STABLE as i32,
            size: OpSize::S64,
        });
        entry.push(MInst::SparseCommit {
            src_offset: SPARSE as i32,
            dst_offset: STABLE as i32,
            byte_size: 8,
            dirty_words_offset: DIRTY as i32,
            dirty_word_count: 1,
            summary_words_offset: SUMMARY as i32,
            summary_word_count: 1,
            four_state: false,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(1));
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: OLD_OUTPUT as i32,
            src: old_stable,
            size: OpSize::S64,
        });
        exit.push(MInst::Return);
        func.push_block(entry);
        func.push_block(exit);

        mir_legalize::legalize(&mut func);
        mir_opt::optimize(&mut func);
        let allocation = regalloc::run_regalloc(&mut func).unwrap();
        let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();

        let hidden_scratch = [
            Register::RAX,
            Register::RCX,
            Register::RDX,
            Register::RSI,
            Register::RDI,
            Register::R8,
            Register::R9,
        ];
        let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
        while decoder.can_decode() {
            let instruction = decoder.decode();
            if matches!(instruction.mnemonic(), Mnemonic::Push | Mnemonic::Pop) {
                assert!(
                    !hidden_scratch.contains(&instruction.op0_register()),
                    "SparseCommit emitted a hidden scratch save/restore: {instruction}"
                );
            }
        }

        let old = 0x0123_4567_89ab_cdefu64;
        let new = 0xfedc_ba98_7654_3210u64;
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0u8; 64];
        state[STABLE..STABLE + 8].copy_from_slice(&old.to_le_bytes());
        state[SPARSE..SPARSE + 8].copy_from_slice(&new.to_le_bytes());
        state[DIRTY..DIRTY + 8].copy_from_slice(&1u64.to_le_bytes());
        state[SUMMARY..SUMMARY + 8].copy_from_slice(&1u64.to_le_bytes());

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[STABLE..STABLE + 8].try_into().unwrap()),
            new
        );
        assert_eq!(
            u64::from_le_bytes(state[OLD_OUTPUT..OLD_OUTPUT + 8].try_into().unwrap()),
            old
        );
        assert_eq!(
            u64::from_le_bytes(state[DIRTY..DIRTY + 8].try_into().unwrap()),
            0
        );
        assert_eq!(
            u64::from_le_bytes(state[SUMMARY..SUMMARY + 8].try_into().unwrap()),
            0
        );
    }

    #[test]
    fn sparse_worklist_deduplicates_regions_and_commits_tail_bytes() {
        const BYTE_SIZE: usize = 13;
        const STABLE: usize = 0;
        const SPARSE: usize = 32;
        const DIRTY: usize = 64;
        const SUMMARY: usize = 72;
        const ACTIVE_BITS: usize = 80;

        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let descriptor = SparseCommitDescriptor {
            src_offset: SPARSE as u64,
            dst_offset: STABLE as u64,
            byte_size: BYTE_SIZE as u64,
            dirty_words_offset: DIRTY as u64,
            dirty_word_count: 1,
            summary_words_offset: SUMMARY as u64,
            summary_word_count: 1,
            four_state: 1,
        };
        let table = func.intern_constant_table(descriptor.words().to_vec());
        let mut block = MBlock::new(BlockId(0));
        for _ in 0..2 {
            block.push(MInst::SparseMarkActive {
                active_index: 0,
                active_bits_offset: ACTIVE_BITS as i32,
                active_capacity: 1,
            });
        }
        block.push(MInst::SparseCommitWorklist {
            descriptor_table: table,
            active_bits_offset: ACTIVE_BITS as i32,
            active_capacity: 1,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0xa5u8; 128];
        state[STABLE..STABLE + BYTE_SIZE * 2].fill(0);
        for index in 0..BYTE_SIZE * 2 {
            state[SPARSE + index] = index as u8 ^ 0x6d;
        }
        state[DIRTY..DIRTY + 8].copy_from_slice(&3u64.to_le_bytes());
        state[SUMMARY..SUMMARY + 8].copy_from_slice(&1u64.to_le_bytes());
        state[ACTIVE_BITS..ACTIVE_BITS + 8].fill(0);
        let stable_sentinel = state[STABLE + BYTE_SIZE * 2];

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            &state[STABLE..STABLE + BYTE_SIZE * 2],
            &state[SPARSE..SPARSE + BYTE_SIZE * 2]
        );
        assert_eq!(state[STABLE + BYTE_SIZE * 2], stable_sentinel);
        assert_eq!(
            u64::from_le_bytes(state[DIRTY..DIRTY + 8].try_into().unwrap()),
            0
        );
        assert_eq!(
            u64::from_le_bytes(state[SUMMARY..SUMMARY + 8].try_into().unwrap()),
            0
        );
        assert_eq!(
            u64::from_le_bytes(state[ACTIVE_BITS..ACTIVE_BITS + 8].try_into().unwrap()),
            0
        );
    }

    #[test]
    fn sparse_worklist_clobbers_are_allocated_and_saved_once_per_function() {
        const INPUT: usize = 0;
        const OUTPUT: usize = 8;
        const ACTIVE_BITS: usize = 16;

        let mut vregs = VRegAllocator::new();
        let live_through = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let descriptor = func.intern_constant_table(vec![0; SparseCommitDescriptor::WORDS]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: live_through,
            base: BaseReg::SimState,
            offset: INPUT as i32,
            size: OpSize::S64,
        });
        entry.push(MInst::SparseCommitWorklist {
            descriptor_table: descriptor,
            active_bits_offset: ACTIVE_BITS as i32,
            active_capacity: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(1));
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: OUTPUT as i32,
            src: live_through,
            size: OpSize::S64,
        });
        exit.push(MInst::Return);
        func.push_block(entry);
        func.push_block(exit);

        mir_legalize::legalize(&mut func);
        mir_opt::optimize(&mut func);
        let allocation = regalloc::run_regalloc(&mut func).unwrap();
        assert!(
            allocation.spill_frame_size >= 8,
            "a value live through an all-GPR clobber needs a stack home"
        );
        let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();

        let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
        let mut pushes = Vec::new();
        let mut pops = Vec::new();
        while decoder.can_decode() {
            let instruction = decoder.decode();
            match instruction.mnemonic() {
                Mnemonic::Push => pushes.push(instruction.op0_register()),
                Mnemonic::Pop => pops.push(instruction.op0_register()),
                _ => {}
            }
        }
        assert!(pushes.is_empty(), "the JIT arena keeps RSP invariant");
        assert!(pops.is_empty(), "the JIT arena keeps RSP invariant");

        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0u8; 32];
        state[INPUT..INPUT + 8].copy_from_slice(&0x0123_4567_89ab_cdefu64.to_le_bytes());
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[OUTPUT..OUTPUT + 8].try_into().unwrap()),
            0x0123_4567_89ab_cdef
        );
    }

    #[test]
    fn sparse_worklist_fast_path_commits_single_chunk_four_state_tail() {
        const BYTE_SIZE: usize = 5;
        const STABLE: usize = 0;
        const SPARSE: usize = 16;
        const DIRTY: usize = 32;
        const SUMMARY: usize = 40;
        const ACTIVE_BITS: usize = 48;

        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let descriptor = SparseCommitDescriptor {
            src_offset: SPARSE as u64,
            dst_offset: STABLE as u64,
            byte_size: BYTE_SIZE as u64,
            dirty_words_offset: DIRTY as u64,
            dirty_word_count: 1,
            summary_words_offset: SUMMARY as u64,
            summary_word_count: 1,
            four_state: 1,
        };
        let table = func.intern_constant_table(descriptor.words().to_vec());
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::SparseMarkActive {
            active_index: 0,
            active_bits_offset: ACTIVE_BITS as i32,
            active_capacity: 1,
        });
        block.push(MInst::SparseCommitWorklist {
            descriptor_table: table,
            active_bits_offset: ACTIVE_BITS as i32,
            active_capacity: 1,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0xa5u8; 80];
        state[STABLE..STABLE + BYTE_SIZE * 2].fill(0);
        for index in 0..BYTE_SIZE * 2 {
            state[SPARSE + index] = 0x31 + index as u8;
        }
        state[DIRTY..DIRTY + 8].copy_from_slice(&1u64.to_le_bytes());
        state[SUMMARY..SUMMARY + 8].copy_from_slice(&1u64.to_le_bytes());
        state[ACTIVE_BITS..ACTIVE_BITS + 8].fill(0);
        let sentinel = state[STABLE + BYTE_SIZE * 2];

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            &state[STABLE..STABLE + BYTE_SIZE * 2],
            &state[SPARSE..SPARSE + BYTE_SIZE * 2]
        );
        assert_eq!(state[STABLE + BYTE_SIZE * 2], sentinel);
        assert_eq!(
            u64::from_le_bytes(state[DIRTY..DIRTY + 8].try_into().unwrap()),
            0
        );
        assert_eq!(
            u64::from_le_bytes(state[SUMMARY..SUMMARY + 8].try_into().unwrap()),
            0
        );
        assert_eq!(
            u64::from_le_bytes(state[ACTIVE_BITS..ACTIVE_BITS + 8].try_into().unwrap()),
            0
        );
    }

    #[test]
    fn sparse_worklist_scans_later_bitmap_words_and_ignores_padding_bits() {
        const CAPACITY: usize = 66;
        const ACTIVE_INDEX: usize = 65;
        const STABLE: usize = 0;
        const SPARSE: usize = 8;
        const DIRTY: usize = 16;
        const SUMMARY: usize = 24;
        const ACTIVE_BITS: usize = 32;

        let descriptor = SparseCommitDescriptor {
            src_offset: SPARSE as u64,
            dst_offset: STABLE as u64,
            byte_size: 8,
            dirty_words_offset: DIRTY as u64,
            dirty_word_count: 1,
            summary_words_offset: SUMMARY as u64,
            summary_word_count: 1,
            four_state: 0,
        };
        let mut rows = vec![0; CAPACITY * SparseCommitDescriptor::WORDS];
        let row = ACTIVE_INDEX * SparseCommitDescriptor::WORDS;
        rows[row..row + SparseCommitDescriptor::WORDS].copy_from_slice(&descriptor.words());

        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let table = func.intern_constant_table(rows);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::SparseMarkActive {
            active_index: ACTIVE_INDEX as u32,
            active_bits_offset: ACTIVE_BITS as i32,
            active_capacity: CAPACITY,
        });
        block.push(MInst::SparseCommitWorklist {
            descriptor_table: table,
            active_bits_offset: ACTIVE_BITS as i32,
            active_capacity: CAPACITY,
        });
        block.push(MInst::Return);
        func.push_block(block);
        func.verify();

        let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0u8; 64];
        state[SPARSE..SPARSE + 8].copy_from_slice(&0xdead_beef_cafe_babeu64.to_le_bytes());
        state[DIRTY..DIRTY + 8].copy_from_slice(&1u64.to_le_bytes());
        state[SUMMARY..SUMMARY + 8].copy_from_slice(&1u64.to_le_bytes());
        // Bit 127 is padding outside CAPACITY and models a malformed restored
        // checkpoint. The generated mark adds valid bit 65 in the same word.
        state[ACTIVE_BITS + 8..ACTIVE_BITS + 16].copy_from_slice(&(1u64 << 63).to_le_bytes());

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[STABLE..STABLE + 8].try_into().unwrap()),
            0xdead_beef_cafe_babe
        );
        assert_eq!(
            u64::from_le_bytes(state[ACTIVE_BITS..ACTIVE_BITS + 8].try_into().unwrap()),
            0
        );
        assert_eq!(
            u64::from_le_bytes(state[ACTIVE_BITS + 8..ACTIVE_BITS + 16].try_into().unwrap()),
            0
        );
    }

    #[test]
    fn materialized_compare_branch_preserves_condition_used_after_the_branch() {
        let mut vregs = VRegAllocator::new();
        let zero = vregs.alloc();
        let alternative = vregs.alloc();
        let condition = vregs.alloc();
        let merged = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        entry.push(MInst::LoadImm {
            dst: alternative,
            value: 7,
        });
        entry.push(MInst::CmpImm {
            dst: condition,
            lhs: zero,
            imm: 0,
            kind: CmpKind::Eq,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut true_block = MBlock::new(BlockId(1));
        true_block.push(MInst::Jump { target: BlockId(3) });
        let mut false_block = MBlock::new(BlockId(2));
        false_block.push(MInst::Jump { target: BlockId(3) });

        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: merged,
            sources: vec![(BlockId(1), condition), (BlockId(2), alternative)],
        });
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: merged,
            size: OpSize::S64,
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, true_block, false_block, join];
        func.verify();

        let allocation = regalloc::run_regalloc(&mut func).unwrap();
        let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0u8; 8];

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(u64::from_le_bytes(state), 1);
    }

    fn execute_variable_shift_boundaries(use_bmi2: bool) {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let count = vregs.alloc();
        let shl = vregs.alloc();
        let shr = vregs.alloc();
        let sar = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        func.target_features = X86Features::for_test(use_bmi2);

        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: lhs,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: count,
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::Shl {
            dst: shl,
            lhs,
            rhs: count,
        });
        block.push(MInst::Shr {
            dst: shr,
            lhs,
            rhs: count,
        });
        block.push(MInst::Sar {
            dst: sar,
            lhs,
            rhs: count,
        });
        for (offset, src) in [(16, shl), (24, shr), (32, sar)] {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset,
                src,
                size: OpSize::S64,
            });
        }
        block.push(MInst::Return);
        func.push_block(block);

        mir_legalize::legalize(&mut func);
        mir_opt::optimize(&mut func);
        assert_eq!(
            func.blocks
                .iter()
                .flat_map(|block| &block.insts)
                .filter(|inst| matches!(inst, MInst::CmpImmSelect { imm: 64, .. }))
                .count(),
            3
        );
        let allocation = regalloc::run_regalloc(&mut func).unwrap();
        let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let lhs_value = 0x8000_0000_0000_0001u64;

        for count_value in [63u64, 64, 65, 127, 128, 129] {
            let mut state = [0u8; 40];
            state[0..8].copy_from_slice(&lhs_value.to_le_bytes());
            state[8..16].copy_from_slice(&count_value.to_le_bytes());
            assert_eq!(unsafe { jit.call(&mut state) }, 0);

            let actual_shl = u64::from_le_bytes(state[16..24].try_into().unwrap());
            let actual_shr = u64::from_le_bytes(state[24..32].try_into().unwrap());
            let actual_sar = u64::from_le_bytes(state[32..40].try_into().unwrap());
            let expected_shl = if count_value >= 64 {
                0
            } else {
                lhs_value << count_value
            };
            let expected_shr = if count_value >= 64 {
                0
            } else {
                lhs_value >> count_value
            };
            let expected_sar = if count_value >= 64 {
                u64::MAX
            } else {
                ((lhs_value as i64) >> count_value) as u64
            };
            assert_eq!(actual_shl, expected_shl, "shl count={count_value}");
            assert_eq!(actual_shr, expected_shr, "shr count={count_value}");
            assert_eq!(actual_sar, expected_sar, "sar count={count_value}");
        }
    }

    #[test]
    fn legacy_variable_shifts_do_not_wrap_large_counts() {
        execute_variable_shift_boundaries(false);
    }

    #[test]
    fn bmi2_variable_shifts_do_not_wrap_large_counts() {
        if !std::is_x86_feature_detected!("bmi2") {
            return;
        }
        execute_variable_shift_boundaries(true);
    }

    #[test]
    fn rip_relative_constant_tables_execute_for_multiple_indexes() {
        let mut vregs = VRegAllocator::new();
        let index = vregs.alloc();
        let byte_index = vregs.alloc();
        let first_addr = vregs.alloc();
        let second_addr = vregs.alloc();
        let first_value = vregs.alloc();
        let second_value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
        let first_values = vec![0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210, 0, u64::MAX];
        let second_values = vec![11, 29, 47, 83];
        let first_table = func.intern_constant_table(first_values.clone());
        let second_table = func.intern_constant_table(second_values.clone());

        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: index,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::ShlImm {
            dst: byte_index,
            src: index,
            imm: 3,
        });
        block.push(MInst::LoadConstantTableAddr {
            dst: first_addr,
            table: first_table,
        });
        block.push(MInst::LoadPtrIndexed {
            dst: first_value,
            ptr: first_addr,
            offset: 0,
            index: byte_index,
            size: OpSize::S64,
        });
        block.push(MInst::LoadConstantTableAddr {
            dst: second_addr,
            table: second_table,
        });
        block.push(MInst::LoadPtrIndexed {
            dst: second_value,
            ptr: second_addr,
            offset: 0,
            index: byte_index,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: first_value,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: second_value,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);

        mir_legalize::legalize(&mut func);
        mir_opt::optimize(&mut func);
        let allocation = regalloc::run_regalloc(&mut func).unwrap();
        let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();

        let trailing_table = second_values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        assert!(emitted.code.ends_with(&trailing_table));
        assert_eq!(
            emitted.code.len() - emitted.text_size,
            (first_values.len() + second_values.len()) * std::mem::size_of::<u64>()
        );
        assert!(emitted.text_size < emitted.code.len());

        let mut decoder =
            Decoder::new(64, &emitted.code[..emitted.text_size], DecoderOptions::NONE);
        let mut table_leas = 0;
        while decoder.can_decode() {
            let instruction = decoder.decode();
            if instruction.mnemonic() == Mnemonic::Lea {
                assert_eq!(instruction.memory_base(), Register::RIP);
                table_leas += 1;
            }
            if instruction.mnemonic() == Mnemonic::Ret {
                break;
            }
        }
        assert_eq!(table_leas, 2);

        let jit = JitCode::new(&emitted.code).unwrap();
        for index_value in 0..first_values.len() {
            let mut state = [0u8; 24];
            state[0..8].copy_from_slice(&(index_value as u64).to_le_bytes());
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(
                u64::from_le_bytes(state[8..16].try_into().unwrap()),
                first_values[index_value]
            );
            assert_eq!(
                u64::from_le_bytes(state[16..24].try_into().unwrap()),
                second_values[index_value]
            );
        }
    }

    #[test]
    fn narrow_immediate_shifts_do_not_use_x86_count_masking() {
        let mut vregs = VRegAllocator::new();
        let src = vregs.alloc();
        let mut results = Vec::new();
        for imm in [31u8, 32, 33, 63] {
            results.push((imm, vregs.alloc(), vregs.alloc()));
        }
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 1 + results.len() * 2]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: src,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S32,
        });
        for (index, (imm, shr, shl)) in results.iter().copied().enumerate() {
            block.push(MInst::ShrImm { dst: shr, src, imm });
            block.push(MInst::ShlImm { dst: shl, src, imm });
            for (column, result) in [shr, shl].into_iter().enumerate() {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: (8 + (index * 16 + column * 8)) as i32,
                    src: result,
                    size: OpSize::S64,
                });
            }
        }
        block.push(MInst::Return);
        func.push_block(block);

        mir_legalize::legalize(&mut func);
        mir_opt::optimize(&mut func);
        let allocation = regalloc::run_regalloc(&mut func).unwrap();
        let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let value = 9u64;
        let mut state = [0u8; 72];
        state[0..8].copy_from_slice(&value.to_le_bytes());
        assert_eq!(unsafe { jit.call(&mut state) }, 0);

        for (index, (imm, _, _)) in results.iter().copied().enumerate() {
            let shr_offset = 8 + index * 16;
            let shl_offset = shr_offset + 8;
            let actual_shr =
                u64::from_le_bytes(state[shr_offset..shr_offset + 8].try_into().unwrap());
            let actual_shl =
                u64::from_le_bytes(state[shl_offset..shl_offset + 8].try_into().unwrap());
            assert_eq!(actual_shr, value >> imm, "shr immediate {imm}");
            assert_eq!(actual_shl, value << imm, "shl immediate {imm}");
        }
    }

    #[test]
    fn packed_lane_eq_executes_for_byte_word_and_dword_slots() {
        const LANES: usize = 32;
        const SCALAR_OFFSET: usize = 128;
        const RESULT_OFFSET: usize = 136;
        const TARGET: u32 = 5;

        for (stride, bit_offset, field_width) in [(1usize, 0usize, 5usize), (2, 3, 7), (4, 9, 9)] {
            let mut vregs = VRegAllocator::new();
            let scalar = vregs.alloc();
            let result = vregs.alloc();
            let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
            let mut block = MBlock::new(BlockId(0));
            block.push(MInst::Load {
                dst: scalar,
                base: BaseReg::SimState,
                offset: SCALAR_OFFSET as i32,
                size: OpSize::S32,
            });
            block.push(MInst::PackedLaneCompare {
                dst: result,
                rhs: PackedLaneCompareRhs::Scalar(scalar),
                kind: CmpKind::Eq,
                offset: 0,
                lane_count: LANES as u8,
                element_stride: stride as u8,
                bit_offset: bit_offset as u8,
                field_width: field_width as u8,
                alias_range: MemoryAliasRange::new(0, LANES * stride),
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: RESULT_OFFSET as i32,
                src: result,
                size: OpSize::S64,
            });
            block.push(MInst::Return);
            func.push_block(block);

            mir_legalize::legalize(&mut func);
            mir_opt::optimize(&mut func);
            let allocation = regalloc::run_regalloc(&mut func).unwrap();
            let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
            let jit = JitCode::new(&emitted.code).unwrap();
            let mut state = [0u8; 144];
            let mut expected = 0u64;
            for lane in 0..LANES {
                let matches = lane % 3 == 1 || lane == 31;
                let field = if matches {
                    TARGET
                } else {
                    (lane as u32 + 7) & ((1 << field_width) - 1)
                };
                let slot = (field << bit_offset) | (u32::MAX << (bit_offset + field_width));
                state[lane * stride..(lane + 1) * stride]
                    .copy_from_slice(&slot.to_le_bytes()[..stride]);
                if matches || field == TARGET {
                    expected |= 1u64 << lane;
                }
            }
            state[SCALAR_OFFSET..SCALAR_OFFSET + 4].copy_from_slice(&TARGET.to_le_bytes());

            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(
                u64::from_le_bytes(state[RESULT_OFFSET..RESULT_OFFSET + 8].try_into().unwrap()),
                expected,
                "stride={stride} bit_offset={bit_offset} field_width={field_width}"
            );
        }
    }

    #[test]
    fn packed_lane_compare_executes_all_relations_for_scalar_and_memory_rhs() {
        const LANES: usize = 16;
        const RHS_OFFSET: usize = 64;
        const SCALAR_OFFSET: usize = 128;
        const RESULT_OFFSET: usize = 136;
        const KINDS: [CmpKind; 10] = [
            CmpKind::Eq,
            CmpKind::Ne,
            CmpKind::LtU,
            CmpKind::LtS,
            CmpKind::LeU,
            CmpKind::LeS,
            CmpKind::GtU,
            CmpKind::GtS,
            CmpKind::GeU,
            CmpKind::GeS,
        ];

        fn relation(kind: CmpKind, lhs: u32, rhs: u32, bits: usize) -> bool {
            let shift = 64 - bits;
            let lhs_signed = ((u64::from(lhs) << shift) as i64) >> shift;
            let rhs_signed = ((u64::from(rhs) << shift) as i64) >> shift;
            match kind {
                CmpKind::Eq => lhs == rhs,
                CmpKind::Ne => lhs != rhs,
                CmpKind::LtU => lhs < rhs,
                CmpKind::LtS => lhs_signed < rhs_signed,
                CmpKind::LeU => lhs <= rhs,
                CmpKind::LeS => lhs_signed <= rhs_signed,
                CmpKind::GtU => lhs > rhs,
                CmpKind::GtS => lhs_signed > rhs_signed,
                CmpKind::GeU => lhs >= rhs,
                CmpKind::GeS => lhs_signed >= rhs_signed,
            }
        }

        for stride in [1usize, 2, 4] {
            let bits = stride * 8;
            let mask = if bits == 32 {
                u32::MAX
            } else {
                (1u32 << bits) - 1
            };
            let scalar_value = (0x8181_8181 & mask).max(1);
            for kind in KINDS {
                for memory_rhs in [false, true] {
                    let mut vregs = VRegAllocator::new();
                    let scalar = vregs.alloc();
                    let result = vregs.alloc();
                    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
                    let mut block = MBlock::new(BlockId(0));
                    if !memory_rhs {
                        block.push(MInst::Load {
                            dst: scalar,
                            base: BaseReg::SimState,
                            offset: SCALAR_OFFSET as i32,
                            size: OpSize::S32,
                        });
                    }
                    block.push(MInst::PackedLaneCompare {
                        dst: result,
                        rhs: if memory_rhs {
                            PackedLaneCompareRhs::Memory {
                                offset: RHS_OFFSET as i32,
                                alias_range: MemoryAliasRange::new(
                                    RHS_OFFSET as i32,
                                    LANES * stride,
                                ),
                            }
                        } else {
                            PackedLaneCompareRhs::Scalar(scalar)
                        },
                        kind,
                        offset: 0,
                        lane_count: LANES as u8,
                        element_stride: stride as u8,
                        bit_offset: 0,
                        field_width: bits as u8,
                        alias_range: MemoryAliasRange::new(0, LANES * stride),
                    });
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: RESULT_OFFSET as i32,
                        src: result,
                        size: OpSize::S64,
                    });
                    block.push(MInst::Return);
                    func.push_block(block);

                    mir_legalize::legalize(&mut func);
                    mir_opt::optimize(&mut func);
                    let allocation = regalloc::run_regalloc(&mut func).unwrap();
                    let emitted =
                        emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
                    let jit = JitCode::new(&emitted.code).unwrap();
                    let mut state = [0u8; 144];
                    let mut expected = 0u64;
                    for lane in 0..LANES {
                        let lhs = ((lane as u32).wrapping_mul(0x31) ^ (mask >> (lane % 5))) & mask;
                        let rhs = if memory_rhs {
                            ((15 - lane) as u32).wrapping_mul(0x27) ^ (1u32 << (bits - 1))
                        } else {
                            scalar_value
                        } & mask;
                        state[lane * stride..(lane + 1) * stride]
                            .copy_from_slice(&lhs.to_le_bytes()[..stride]);
                        if memory_rhs {
                            let start = RHS_OFFSET + lane * stride;
                            state[start..start + stride]
                                .copy_from_slice(&rhs.to_le_bytes()[..stride]);
                        }
                        if relation(kind, lhs, rhs, bits) {
                            expected |= 1u64 << lane;
                        }
                    }
                    state[SCALAR_OFFSET..SCALAR_OFFSET + 4]
                        .copy_from_slice(&scalar_value.to_le_bytes());

                    assert_eq!(unsafe { jit.call(&mut state) }, 0);
                    assert_eq!(
                        u64::from_le_bytes(
                            state[RESULT_OFFSET..RESULT_OFFSET + 8].try_into().unwrap()
                        ),
                        expected,
                        "stride={stride} kind={kind:?} memory_rhs={memory_rhs}"
                    );
                }
            }
        }
    }

    #[test]
    fn packed_byte_affine_compare_executes_all_relations_and_wraps() {
        const BASE_OFFSET: usize = 0;
        const RHS_OFFSET: usize = 1;
        const RESULT_OFFSET: usize = 8;
        const KINDS: [CmpKind; 10] = [
            CmpKind::Eq,
            CmpKind::Ne,
            CmpKind::LtU,
            CmpKind::LtS,
            CmpKind::LeU,
            CmpKind::LeS,
            CmpKind::GtU,
            CmpKind::GtS,
            CmpKind::GeU,
            CmpKind::GeS,
        ];

        fn relation(kind: CmpKind, lhs: u8, rhs: u8) -> bool {
            match kind {
                CmpKind::Eq => lhs == rhs,
                CmpKind::Ne => lhs != rhs,
                CmpKind::LtU => lhs < rhs,
                CmpKind::LtS => (lhs as i8) < (rhs as i8),
                CmpKind::LeU => lhs <= rhs,
                CmpKind::LeS => (lhs as i8) <= (rhs as i8),
                CmpKind::GtU => lhs > rhs,
                CmpKind::GtS => (lhs as i8) > (rhs as i8),
                CmpKind::GeU => lhs >= rhs,
                CmpKind::GeS => (lhs as i8) >= (rhs as i8),
            }
        }

        for kind in KINDS {
            let mut vregs = VRegAllocator::new();
            let base = vregs.alloc();
            let rhs = vregs.alloc();
            let result = vregs.alloc();
            let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
            let mut block = MBlock::new(BlockId(0));
            block.push(MInst::Load {
                dst: base,
                base: BaseReg::SimState,
                offset: BASE_OFFSET as i32,
                size: OpSize::S8,
            });
            block.push(MInst::Load {
                dst: rhs,
                base: BaseReg::SimState,
                offset: RHS_OFFSET as i32,
                size: OpSize::S8,
            });
            block.push(MInst::PackedByteAffineCompare {
                dst: result,
                base,
                rhs,
                kind,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: RESULT_OFFSET as i32,
                src: result,
                size: OpSize::S64,
            });
            block.push(MInst::Return);
            func.push_block(block);

            mir_legalize::legalize(&mut func);
            mir_opt::optimize(&mut func);
            let allocation = regalloc::run_regalloc(&mut func).unwrap();
            let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
            let jit = JitCode::new(&emitted.code).unwrap();
            for (base_value, rhs_value) in [(0u8, 7u8), (120, 128), (248, 3), (255, 255)] {
                let mut state = [0u8; 16];
                state[BASE_OFFSET] = base_value;
                state[RHS_OFFSET] = rhs_value;
                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                let actual =
                    u64::from_le_bytes(state[RESULT_OFFSET..RESULT_OFFSET + 8].try_into().unwrap());
                let expected = (0..16).fold(0u64, |mask, lane| {
                    mask | (u64::from(relation(kind, base_value.wrapping_add(lane), rhs_value))
                        << lane)
                });
                assert_eq!(
                    actual, expected,
                    "kind={kind:?} base={base_value} rhs={rhs_value}"
                );
            }
        }
    }
}
