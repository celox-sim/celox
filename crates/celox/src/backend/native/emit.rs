//! x86-64 code emission: MIR + physical register assignment → machine code.
//!
//! Uses iced-x86's CodeAssembler for instruction encoding.
//! ABI: System V AMD64 — sim state base in RDI (moved to R15 in prologue).
//! Function signature: `fn(unified_mem: *mut u8) -> i64`

use std::collections::HashMap;
use std::fmt;

use iced_x86::BlockEncoderOptions;
use iced_x86::code_asm::*;

use crate::backend::native::features::VariableShiftEncoding;
use crate::backend::native::mir::*;
use crate::backend::native::regalloc::assignment::{AssignmentMap, PhysReg, PhysRegSet};
use crate::backend::native::ssa_destroy::{
    EdgeCopyPlan, ParallelCopyDestination, ParallelCopyOperation, ParallelCopySource,
    SsaDestructionPlan,
};

pub use crate::backend::native::ssa_destroy::SsaDestructionError;

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
            let src = mem_operand_indexed(BaseReg::SimState, src_offset + copied as i32, index);
            let dst = mem_operand_indexed(BaseReg::SimState, dst_offset + copied as i32, index);
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
    Mir(crate::backend::native::mir_verify::MirVerifyError),
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
        error: crate::ir::verify::SirVerifyError,
    },
    Mir {
        phase: &'static str,
        error: crate::backend::native::mir_verify::MirVerifyError,
    },
    Regalloc(crate::backend::native::regalloc::RegallocError),
    Input(EmitInputError),
    SsaDestruction(SsaDestructionError),
    Assembly(IcedError),
}

impl fmt::Display for ChainedEmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sir { phase, error } => write!(f, "{phase}: {error}"),
            Self::Mir { phase, error } => write!(f, "{phase}: {error}"),
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
            Self::Regalloc(error) => Some(error),
            Self::Input(error) => Some(error),
            Self::SsaDestruction(error) => Some(error),
            Self::Assembly(error) => Some(error),
        }
    }
}

impl From<crate::backend::native::regalloc::RegallocError> for ChainedEmitError {
    fn from(error: crate::backend::native::regalloc::RegallocError) -> Self {
        Self::Regalloc(error)
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
                let stack_adjustment = if temporary_live { 8 } else { 0 };
                emit_single_parallel_copy(asm, destination, source, stack_adjustment)?;
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
                        asm.push(preg_to_reg64(register))?
                    }
                    ParallelCopyDestination::Stack(slot) => {
                        let offset = checked_parallel_copy_offset(slot, 0)?;
                        asm.push(qword_ptr(mem_operand(BaseReg::StackFrame, offset)))?;
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
                        asm.pop(preg_to_reg64(register))?
                    }
                    ParallelCopyDestination::Stack(slot) => {
                        // POP computes an RSP-based destination after advancing
                        // RSP, so this uses the unadjusted frame displacement.
                        let offset = checked_parallel_copy_offset(slot, 0)?;
                        asm.pop(qword_ptr(mem_operand(BaseReg::StackFrame, offset)))?;
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
    ) -> Self {
        let mut labels = Vec::new();
        let mut canonical = HashMap::new();

        for (index, block) in func.blocks.iter().enumerate().rev() {
            let next = func.blocks.get(index + 1).map(|next| next.id);
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
        MInst::MemCopy { byte_len: 0, .. } => true,
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
            emit_condition_jump(asm, true_label, condition, true)?;
            if next_block != Some(false_block) {
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
    verify_emission_inputs(func, assignment, spill_frame_size)?;
    let plan = SsaDestructionPlan::build(func, assignment)?;
    plan.verify(func, assignment, spill_frame_size)?;
    emit_planned(func, assignment, spill_frame_size, &plan)
}

/// Emit using the allocation phase's explicit SSA destruction artifact.
/// Verification is intentionally repeated immediately before encoding so a
/// stale or accidentally modified plan cannot reach the emitter.
pub(crate) fn emit_with_plan(
    func: &MFunction,
    assignment: &AssignmentMap,
    spill_frame_size: u32,
    plan: &SsaDestructionPlan,
) -> Result<EmitResult, EmitError> {
    verify_emission_inputs(func, assignment, spill_frame_size)?;
    plan.verify(func, assignment, spill_frame_size)?;
    emit_planned(func, assignment, spill_frame_size, plan)
}

fn verify_emission_inputs(
    func: &MFunction,
    assignment: &AssignmentMap,
    spill_frame_size: u32,
) -> Result<(), EmitError> {
    func.verify_result().map_err(EmitError::Mir)?;

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

    // x86-64's `sub rsp, imm32` encodes a signed immediate.  Include the
    // alignment padding and callee-save pushes in the proof rather than
    // allowing a large (but otherwise well-formed) frame to wrap at encoding.
    checked_frame_size(spill_frame_size, used_callee_saved(assignment).len())?;
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

fn checked_frame_size(
    spill_frame_size: u32,
    callee_saved_count: usize,
) -> Result<u32, EmitInputError> {
    let callee_push_size = u32::try_from(callee_saved_count)
        .ok()
        .and_then(|count| count.checked_mul(8))
        .ok_or_else(|| {
            EmitInputError::new(
                "EMIT.FRAME_SIZE_RANGE",
                None,
                None,
                None,
                "callee-save area exceeds the addressable native stack frame",
            )
        })?;
    let total_push = callee_push_size.checked_add(8).ok_or_else(|| {
        EmitInputError::new(
            "EMIT.FRAME_SIZE_RANGE",
            None,
            None,
            None,
            "native prologue size overflow",
        )
    })?;
    let misalignment = total_push.checked_add(spill_frame_size).ok_or_else(|| {
        EmitInputError::new(
            "EMIT.FRAME_SIZE_RANGE",
            None,
            None,
            None,
            "spill frame plus native prologue exceeds u32",
        )
    })? % 16;
    let padding = if misalignment == 0 {
        0
    } else {
        16 - misalignment
    };
    let frame_size = spill_frame_size.checked_add(padding).ok_or_else(|| {
        EmitInputError::new(
            "EMIT.FRAME_SIZE_RANGE",
            None,
            None,
            None,
            "aligned spill frame exceeds u32",
        )
    })?;
    i32::try_from(frame_size).map_err(|_| {
        EmitInputError::new(
            "EMIT.FRAME_SIZE_RANGE",
            None,
            None,
            None,
            "aligned spill frame exceeds signed 32-bit x86 addressing",
        )
    })?;
    Ok(frame_size)
}

fn emit_planned(
    func: &MFunction,
    assignment: &AssignmentMap,
    spill_frame_size: u32,
    plan: &SsaDestructionPlan,
) -> Result<EmitResult, EmitError> {
    let mut asm = CodeAssembler::new(64)?;

    // Empty layout fallthrough chains share the label of the next block that
    // emits code. iced permits only one label on an instruction, so distinct
    // BlockIds at the same machine-code IP must be aliases here rather than
    // zero-length pseudo instructions in the assembler stream.
    let mut block_labels = BlockLabels::new(&mut asm, func, assignment, plan);
    let mut constant_table_labels = func
        .constant_tables()
        .iter()
        .map(|_| asm.create_label())
        .collect::<Vec<_>>();

    let callee_saved = used_callee_saved(assignment);
    let frame_size = checked_frame_size(spill_frame_size, callee_saved.len())?;

    let mut epilogue_label = asm.create_label();
    let use_counts = count_vreg_uses(func, plan);

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

        // Pre-scan: detect Cmp+Branch fusion opportunity.
        // If the instruction immediately before Branch is Cmp/CmpImm,
        // and the cmp result is only used by the Branch, we can fuse
        // into cmp + jcc (skipping setcc + movzx + test).
        let fused_cmp: Option<VReg> = if block.insts.len() >= 2 {
            if let Some(MInst::Branch { cond, .. }) = block.terminator() {
                let pre = &block.insts[block.insts.len() - 2];
                let is_cmp = pre.def() == Some(*cond)
                    && matches!(pre, MInst::Cmp { .. } | MInst::CmpImm { .. });
                if is_cmp {
                    // A condition can remain live through a successor phi even
                    // when Branch is its only use in this block.  In that case
                    // setcc must still materialize the SSA value for the edge.
                    if use_counts.get(cond).copied() == Some(1) {
                        Some(*cond)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let fallthrough_continuation = next_block_id
            .filter(|&next| {
                matches!(block.terminator(), Some(MInst::Jump { target }) if *target == next)
                    && !plan
                        .edge(block.id, next)
                        .is_some_and(|edge| edge.has_effective_copies())
            })
            .and_then(|next| {
                block.insts[..block.insts.len() - 1]
                    .iter()
                    .rposition(|inst| !instruction_emits_no_code(inst, assignment))
                    .map(|instruction| (instruction, next))
            })
            .map(|(instruction, next)| block_labels.index(next).map(|label| (instruction, label)))
            .transpose()?;

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
                        emit_branch_with_edge_copies(
                            &mut asm,
                            &block_labels,
                            plan,
                            block.id,
                            *true_bb,
                            *false_bb,
                            next_block_id,
                            EmittedBranchCondition::Compare(kind),
                        )?;
                    } else {
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
                    } // end else (non-fused branch)
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
                            Some(block_labels.label_mut(index)),
                        )?
                    } else {
                        emit_inst(
                            &mut asm,
                            inst,
                            assignment,
                            func,
                            &constant_table_labels,
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
    asm.set_label(&mut epilogue_label)?;
    if frame_size > 0 {
        asm.add(rsp, frame_size as i32)?;
    }
    asm.pop(SIM_BASE)?;
    for &reg in callee_saved.iter().rev() {
        asm.pop(preg_to_reg64(reg))?;
    }
    asm.ret()?;

    // Keep immutable lookup data out of every control-flow path. Table
    // addresses are encoded RIP-relatively, so the resulting code remains
    // relocatable when copied into executable memory by the JIT.
    for (label, table) in constant_table_labels.iter_mut().zip(func.constant_tables()) {
        asm.set_label(label)?;
        asm.dq(table)?;
    }

    let result = asm.assemble_options(0x0, BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS)?;
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
    Ok(EmitResult {
        code: result.inner.code_buffer,
        frame_size,
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
    constant_table_labels: &[CodeLabel],
    continuation_label: Option<&mut CodeLabel>,
) -> Result<bool, IcedError> {
    let mut bound_continuation = false;
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
                return Ok(false);
            }
            let qwords = byte_len / 8;
            let rem = byte_len % 8;
            if rem != 0 {
                asm.push(rax)?;
            }
            if qwords != 0 {
                asm.push(rcx)?;
            }
            asm.push(rsi)?;
            asm.push(rdi)?;
            asm.lea(rsi, mem_operand(BaseReg::SimState, *src_offset))?;
            asm.lea(rdi, mem_operand(BaseReg::SimState, *dst_offset))?;
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
            asm.pop(rdi)?;
            asm.pop(rsi)?;
            if qwords != 0 {
                asm.pop(rcx)?;
            }
            if rem != 0 {
                asm.pop(rax)?;
            }
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
            // This pseudo has no MIR operands.  Preserve every scratch register
            // so values allocated across the commit remain intact.
            for reg in [rax, rcx, rdx, rsi, rdi, r8, r9] {
                asm.push(reg)?;
            }
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
                let mut summary_done = asm.create_label();
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
                    )),
                )?;
                asm.mov(
                    qword_ptr(mem_operand_indexed(
                        BaseReg::SimState,
                        *dirty_words_offset,
                        rdi,
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
                asm.set_label(&mut summary_done)?;
            }
            for reg in [r9, r8, rdi, rsi, rdx, rcx, rax] {
                asm.pop(reg)?;
            }
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
            // A 32-bit x86 shift masks the count modulo 32. MIR shifts are
            // 64-bit word operations, so counts 32..63 must use the 64-bit
            // encoding even when the source's upper half is known zero.
            if func.is_narrow32(*src) && *imm < 32 {
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
            if func.is_narrow32(*dst) && *imm < 32 {
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
        MInst::Branch { .. } | MInst::Jump { .. } => {
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
                // Shift a saved copy in place and pop the result into RCX, so
                // the original lhs register remains untouched.
                asm.push(l)?;
                match op {
                    ShiftOp::Shr => asm.shr(qword_ptr(rsp), cl)?,
                    ShiftOp::Shl => asm.shl(qword_ptr(rsp), cl)?,
                    ShiftOp::Sar => asm.sar(qword_ptr(rsp), cl)?,
                }
                asm.pop(rcx)?;
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
        asm.push(r)?;
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
            asm.idiv(qword_ptr(rsp))?;
        } else {
            asm.div(qword_ptr(rsp))?;
        }
        asm.add(rsp, 8)?;
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
) -> Result<EmitResult, ChainedEmitError> {
    use super::{isel, regalloc};
    let timing = std::env::var_os("CELOX_PHASE_TIMING").is_some();
    let mir_stats = std::env::var_os("CELOX_MIR_STATS").is_some();
    let copy_stats = timing
        || mir_stats
        || std::env::var_os("CELOX_REGALLOC_TIMING").is_some()
        || std::env::var_os("CELOX_REGALLOC_STATS").is_some();
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
        (merged, boundaries)
    } else {
        (units[0].clone(), vec![])
    };
    sir_eu
        .verify_result()
        .map_err(|error| ChainedEmitError::Sir {
            phase: "after native SIR merge",
            error,
        })?;
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
    if timing {
        eprintln!("[native-timing] emit_chained verify after_isel label={label}");
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
        eprintln!(
            "[native-timing] emit_chained legalize mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    if timing {
        eprintln!("[native-timing] emit_chained verify after_legalize label={label}");
    }
    mfunc
        .verify_result()
        .map_err(|error| ChainedEmitError::Mir {
            phase: "after MIR legalization",
            error,
        })?;
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
    if timing {
        eprintln!("[native-timing] emit_chained verify after_mir_opt label={label}");
    }
    mfunc
        .verify_result()
        .map_err(|error| ChainedEmitError::Mir {
            phase: "after MIR optimization",
            error,
        })?;
    let regalloc_start = timing.then(crate::timing::now);
    let ra = regalloc::run_regalloc_with_label(&mut mfunc, label)?;
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
    if copy_stats {
        let stats = ra.ssa_destruction.stats();
        eprintln!(
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
    mfunc
        .verify_result()
        .map_err(|error| ChainedEmitError::Mir {
            phase: "after post-allocation MIR peepholes",
            error,
        })?;
    if mir_stats {
        log_mir_stats(label, "after_regalloc", &mfunc);
    }
    if std::env::var_os("CELOX_MIR_BLOCK_STATS").is_some() {
        log_mir_block_stats(label, "after_regalloc", &mfunc);
    }
    dump_native_block_context(label, "after_regalloc", &sir_eu, &mfunc);
    let emit_start = timing.then(crate::timing::now);
    let result = emit_with_plan(
        &mfunc,
        &ra.assignment,
        ra.spill_frame_size,
        &ra.ssa_destruction,
    )?;
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
                MInst::LoadImm { .. } | MInst::LoadConstantTableAddr { .. } => imm += 1,
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
                MInst::MemCopy { .. } | MInst::SparseCommit { .. } => memcopy += 1,
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
                MInst::UDiv { .. }
                | MInst::URem { .. }
                | MInst::SDiv { .. }
                | MInst::SRem { .. } => div_rem += 1,
                MInst::BitNot { .. }
                | MInst::Neg { .. }
                | MInst::Popcnt { .. }
                | MInst::Bsr { .. }
                | MInst::BsrOr { .. }
                | MInst::Pext { .. }
                | MInst::Pdep { .. } => bit_ops += 1,
                MInst::Select { .. }
                | MInst::CmpSelect { .. }
                | MInst::CmpImmSelect { .. }
                | MInst::GuardedCmpSelect { .. } => select += 1,
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
                    | MInst::StorePtrIndexed { .. }
                    | MInst::ReleaseStorePtrIndexed { .. } => indexed_mem += 1,
                    MInst::MemCopy { .. } => memcopy += 1,
                    MInst::LoadImm { .. } | MInst::LoadConstantTableAddr { .. } => imm += 1,
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
                    MInst::BitNot { .. }
                    | MInst::Neg { .. }
                    | MInst::Popcnt { .. }
                    | MInst::Bsr { .. }
                    | MInst::BsrOr { .. }
                    | MInst::Pext { .. }
                    | MInst::Pdep { .. } => bit_ops += 1,
                    MInst::Select { .. }
                    | MInst::CmpSelect { .. }
                    | MInst::CmpImmSelect { .. }
                    | MInst::GuardedCmpSelect { .. } => select += 1,
                    MInst::Branch { .. }
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
        eprintln!(
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
    eu: &crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>,
    func: &super::mir::MFunction,
) {
    let Some(raw) = std::env::var_os("CELOX_NATIVE_DUMP_BLOCK") else {
        return;
    };
    if let Some(raw_label) = std::env::var_os("CELOX_NATIVE_DUMP_LABEL")
        && raw_label != label
    {
        return;
    }
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

#[cfg(test)]
mod shift_encoding_tests {
    use super::*;
    use crate::backend::native::features::X86Features;
    use crate::backend::native::jit_mem::JitCode;
    use crate::backend::native::{mir_legalize, mir_opt, regalloc};
    use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, Register};

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
    fn legacy_shift_with_rcx_destination_uses_a_stack_copy() {
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
            vec![Mnemonic::Push, Mnemonic::Shl, Mnemonic::Pop]
        );
        assert_eq!(instructions[0].op0_register(), Register::R8);
        assert_eq!(instructions[1].memory_base(), Register::RSP);
        assert_eq!(instructions[1].op1_register(), Register::CL);
        assert_eq!(instructions[2].op0_register(), Register::RCX);
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
    fn compare_branch_fusion_preserves_condition_used_after_the_branch() {
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

        let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
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
}
