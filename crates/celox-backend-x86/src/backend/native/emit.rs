//! x86-64 code emission: MIR + physical register assignment → machine code.
//!
//! Uses iced-x86's CodeAssembler for instruction encoding.
//! ABI: System V AMD64 at the external boundary. On supported x86-64 hosts,
//! generated code temporarily uses an otherwise available segment base for
//! simulation-state/allocator-arena addressing, leaving every non-stack GPR
//! available to allocation. Other hosts reserve R15 as the state base.
//! Function signature: `fn(unified_mem: *mut u8) -> i64`
//!
//! This module owns the emission context, public result/error types, and function
//! assembly. Child modules handle instruction encoding, operand forms, control
//! flow, memory operations, the compilation pipeline, and diagnostic logging.

mod arithmetic;
mod control_flow;
mod diagnostics;
mod folded_operands;
mod instruction;
mod memory;
mod operands;
mod pipeline;

use arithmetic::{
    BinOp, DivOp, ShiftOp, emit_and_imm, emit_and_memory_imm, emit_binop_memory, emit_binop_rr,
    emit_cmp_imm_select, emit_cmp_select, emit_divrem, emit_guarded_cmp_select, emit_inverse_jcc,
    emit_jcc, emit_or_imm64, emit_select_memory, emit_setcc, emit_shift,
};
use control_flow::{
    BlockLabels, EmittedBranchCondition, branch_label, emission_block_order, emit_branch_predicate,
    emit_branch_with_edge_copies, emit_parallel_copy_plan, instruction_emits_no_code,
};
use diagnostics::{
    dump_native_block_context, log_mir_block_stats, log_mir_stats, log_sir_width_stats,
    mir_inst_count,
};
use folded_operands::{count_vreg_uses, try_emit_load_fold, try_emit_not_and_fold};
use instruction::emit_inst;
use memory::{
    emit_direct_memcopy, emit_sparse_chunk_copy, emit_sparse_commit_worklist,
    emit_sparse_inline_commits,
};
use operands::{
    emit_state_base, mem_operand, mem_operand_indexed, mem_operand_ptr, mem_operand_ptr_indexed,
    preg_to_reg8, preg_to_reg16, preg_to_reg32, preg_to_reg64, resolve, scratch_operand,
    x86_vec_to_xmm,
};
pub use pipeline::{emit_owned_prepared_eu, emit_prepared_eu};

use std::cell::Cell;
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
use crate::{
    HashMap, HashSet, STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET,
    STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET,
};
use celox_state_layout::STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET;

pub use crate::native::ssa_destroy::SsaDestructionError;

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
    let mut access_counts = HashMap::<i32, usize>::default();
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

    let mut edge_slots = HashSet::<i32>::default();
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
    /// Host feature bits actually required by instructions in this function.
    pub required_image_features: u8,
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
    /// The compile observed its cancellation token; not a backend defect.
    Cancelled,
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
            Self::Cancelled => f.write_str("compilation was cancelled"),
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
            Self::Cancelled => None,
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
    let mut uses_bmi2 = false;
    let mut uses_avx = false;
    let mut uses_popcnt = false;
    for inst in func.blocks.iter().flat_map(|block| &block.insts) {
        uses_bmi2 |= matches!(inst, MInst::Pext { .. } | MInst::Pdep { .. })
            || matches!(
                inst,
                MInst::Shr { .. } | MInst::Shl { .. } | MInst::Sar { .. }
            ) && matches!(
                func.target_features.variable_shift_encoding(),
                VariableShiftEncoding::Bmi2
            );
        uses_avx |= func.target_features.avx()
            && matches!(
                inst,
                MInst::X86Simd(
                    X86SimdInst::Zero128 { .. }
                        | X86SimdInst::Load128 { .. }
                        | X86SimdInst::Binary128 { .. }
                        | X86SimdInst::Store128 { .. }
                )
            );
        uses_popcnt |= matches!(inst, MInst::Popcnt { .. });
    }
    let mut required_image_features = super::features::emitted_image_feature_bits(
        func.target_features,
        uses_bmi2,
        uses_avx,
        uses_popcnt,
    );
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
    let mut jump_table_labels = HashMap::<(BlockId, usize), CodeLabel>::default();
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
                    if func.target_features.bmi1()
                        && inst_idx + 1 < block.insts.len()
                        && try_emit_not_and_fold(
                            &mut asm,
                            inst,
                            &block.insts[inst_idx + 1],
                            &use_counts,
                            assignment,
                        )?
                    {
                        required_image_features |= super::features::IMAGE_FEATURE_BMI1;
                        inst_idx += 2;
                        continue;
                    }
                    if inst_idx + 1 < block.insts.len()
                        && try_emit_load_fold(
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
        required_image_features,
    })
}

#[cfg(test)]
mod tests;
