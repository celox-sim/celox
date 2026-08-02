//! AArch64 emission for the transitional scalar MIR pipeline.

use std::collections::HashMap;
use std::fmt;

use celox_backend_x86::native::mir::MFunction as LegacyFunction;
use celox_backend_x86::native::regalloc::AssignmentMap as LegacyAssignment;
use celox_backend_x86::native::scalar_pipeline::{
    ScalarPrepareError as PrepareError, prepare_scalar_mir,
};
use celox_state_layout::{
    STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET, STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET,
    STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
};
use dynasmrt::aarch64::Aarch64Relocation;
use dynasmrt::{DynamicLabel, DynasmApi, DynasmError, DynasmLabelApi, VecAssembler, dynasm};

use crate::Arm64Reg;
use crate::allocation::{Assignment, CopyDestination, CopyOperation, CopySource, EdgeCopyPlan};
use crate::mir::{
    BaseReg, BlockId, BranchPredicate, CmpKind, MFunction, MInst, OpSize, PackedLaneCompareRhs,
    SPARSE_COMMIT_DESCRIPTOR_WORDS, VReg,
};

const STATE_REG: u8 = 0;
const SCRATCH0: u8 = 16;
const SCRATCH1: u8 = 17;
const TEMPORARY_BYTES: usize = 16;

/// Result of scalar AArch64 emission.
pub struct EmitResult {
    pub code: Vec<u8>,
    pub text_size: usize,
    pub frame_size: u32,
    pub required_state_size: u32,
    pub block_offsets: Vec<(BlockId, u64)>,
}

/// Exact intermediate forms captured by the native runtime.
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

#[derive(Debug)]
pub enum EmitError {
    Assembly(DynasmError),
    MissingAssignment(u32),
    Range(&'static str),
    Unsupported(&'static str),
    LegacyLowering(String),
}

impl fmt::Display for EmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Assembly(error) => error.fmt(formatter),
            Self::MissingAssignment(value) => {
                write!(
                    formatter,
                    "ARM64 MIR value v{value} has no physical assignment"
                )
            }
            Self::Range(message) => write!(formatter, "ARM64 emission range error: {message}"),
            Self::Unsupported(instruction) => {
                write!(
                    formatter,
                    "ARM64 emission does not yet support {instruction}"
                )
            }
            Self::LegacyLowering(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for EmitError {}

impl From<DynasmError> for EmitError {
    fn from(error: DynasmError) -> Self {
        Self::Assembly(error)
    }
}

impl From<crate::legacy_allocation::LegacyLoweringError> for EmitError {
    fn from(error: crate::legacy_allocation::LegacyLoweringError) -> Self {
        match error {
            crate::legacy_allocation::LegacyLoweringError::Unsupported(instruction) => {
                Self::Unsupported(instruction)
            }
            error => Self::LegacyLowering(error.to_string()),
        }
    }
}

#[derive(Debug)]
pub enum ChainedEmitError {
    Prepare(PrepareError),
    Emit(EmitError),
}

impl fmt::Display for ChainedEmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prepare(error) => error.fmt(formatter),
            Self::Emit(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ChainedEmitError {}

impl From<PrepareError> for ChainedEmitError {
    fn from(error: PrepareError) -> Self {
        Self::Prepare(error)
    }
}

impl From<EmitError> for ChainedEmitError {
    fn from(error: EmitError) -> Self {
        Self::Emit(error)
    }
}

/// Lower one prepared SIR execution unit through scalar MIR and AArch64 codegen.
pub fn emit_prepared_eu(
    sir_eu: &celox_backend_x86::ExecutionUnit<celox_backend_x86::RegionedAbsoluteAddr>,
    layout: &celox_backend_x86::MemoryLayout,
    four_state: bool,
    label: &str,
    _x86_options: &celox_backend_x86::X86BackendOptions,
    mut trace: Option<&mut NativeFunctionTrace>,
) -> Result<EmitResult, ChainedEmitError> {
    let tick_loop =
        label == "eval_comb_apply_ff" && celox_backend_x86::native::native_tick_loop_enabled();
    let check_runtime_events = tick_loop
        && sir_eu.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    celox_backend_x86::SIRInstruction::RuntimeEvent { .. }
                        | celox_backend_x86::SIRInstruction::CombCaptureEvent { .. }
                )
            })
        });
    if let Some(trace) = trace.as_deref_mut() {
        trace.optimized_sir = sir_eu.to_string();
    }
    let prepared = prepare_scalar_mir(sir_eu, layout, four_state)?;
    let state_size = prepared.state_size();
    let function = crate::legacy_allocation::lower(&prepared.function).map_err(EmitError::from)?;
    let allocation = crate::regalloc::allocate_with_spills(function)
        .map_err(|error| EmitError::LegacyLowering(error.to_string()))?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.mir_before_regalloc = prepared.function.to_string();
        trace.mir_after_regalloc = format!("{:#?}", allocation.allocated.function);
        let mut assignments = allocation
            .allocated
            .assignment
            .iter()
            .map(|(&value, register)| (value, register.number()))
            .collect::<Vec<_>>();
        assignments.sort_unstable();
        trace.register_assignment = assignments
            .into_iter()
            .map(|(value, register)| format!("  {value} -> x{register}\n"))
            .collect();
        trace.spill_frame_size = allocation.spill_frame_size;
    }
    let result = emit_function(
        &allocation.allocated.function,
        &allocation.allocated.assignment,
        allocation.spill_frame_size,
        state_size,
        &allocation.allocated.edge_copies,
        tick_loop,
        check_runtime_events,
    )?;
    if let Some(trace) = trace {
        trace.disassembly = disassemble(&result.code[..result.text_size], 0);
    }
    Ok(result)
}

/// Emit an already allocated MIR function. Primarily used by focused tests.
pub fn emit(
    function: &LegacyFunction,
    assignment: &LegacyAssignment,
    spill_frame_size: u32,
) -> Result<EmitResult, EmitError> {
    let allocated = crate::legacy_allocation::adapt(function, assignment, spill_frame_size)?;
    emit_function(
        &allocated.function,
        &allocated.assignment,
        spill_frame_size,
        4096,
        &allocated.edge_copies,
        false,
        false,
    )
}

fn resolve(assignment: &Assignment<VReg>, value: VReg) -> Result<u8, EmitError> {
    assignment
        .get(&value)
        .map(Arm64Reg::number)
        .ok_or(EmitError::MissingAssignment(value.0))
}

fn align16(value: usize) -> Result<usize, EmitError> {
    value
        .checked_add(15)
        .map(|value| value & !15)
        .ok_or(EmitError::Range("native arena alignment overflow"))
}

fn emit_function(
    function: &MFunction,
    assignment: &Assignment<VReg>,
    spill_frame_size: u32,
    state_size: usize,
    plan: &EdgeCopyPlan<BlockId>,
    tick_loop: bool,
    check_runtime_events: bool,
) -> Result<EmitResult, EmitError> {
    let spill_base = align16(state_size)?;
    let temporary_offset = spill_base
        .checked_add(spill_frame_size as usize)
        .and_then(|value| value.checked_add(15))
        .map(|value| value & !15)
        .ok_or(EmitError::Range("spill arena overflow"))?;
    let required_state_size = temporary_offset
        .checked_add(TEMPORARY_BYTES)
        .ok_or(EmitError::Range("native arena size overflow"))?;
    let required_state_size = u32::try_from(required_state_size)
        .map_err(|_| EmitError::Range("native arena exceeds u32"))?;

    let mut ops = VecAssembler::<Aarch64Relocation>::new(0);
    let block_labels = function
        .blocks
        .iter()
        .map(|block| (block.id, ops.new_dynamic_label()))
        .collect::<HashMap<_, _>>();
    let table_labels = function
        .constant_tables()
        .iter()
        .map(|_| ops.new_dynamic_label())
        .collect::<Vec<_>>();
    let epilogue = ops.new_dynamic_label();
    let tick_success = tick_loop.then(|| ops.new_dynamic_label());
    let tick_entry = tick_loop.then(|| block_labels[&function.blocks[0].id]);
    let mut block_offsets = Vec::with_capacity(function.blocks.len());
    let mut callee_saved = assignment
        .iter()
        .map(|(_, register)| register.number())
        .filter(|register| (19..=28).contains(register))
        .collect::<Vec<_>>();
    callee_saved.sort_unstable();
    callee_saved.dedup();

    dynasm!(ops
        ; .arch aarch64
        ; str x30, [sp, #-16]!
    );
    for &register in &callee_saved {
        dynasm!(ops ; .arch aarch64 ; str X(register), [sp, #-16]!);
    }
    if tick_loop {
        // d29 retains the simulator-state pointer across success/error return
        // values. d31 carries the remaining tick count between body entries.
        dynasm!(ops ; .arch aarch64 ; fmov d29, x0);
        emit_address(
            &mut ops,
            STATE_REG,
            STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET as i64,
        );
        emit_load(&mut ops, SCRATCH0, SCRATCH0, OpSize::S64);
        let count_ready = ops.new_dynamic_label();
        dynasm!(ops
            ; .arch aarch64
            ; cbnz x16, =>count_ready
            ; mov x16, #1
            ; =>count_ready
            ; fmov d31, x16
        );
        if check_runtime_events {
            emit_address(
                &mut ops,
                STATE_REG,
                STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET as i64,
            );
            emit_load(&mut ops, SCRATCH0, SCRATCH0, OpSize::S64);
            emit_load(&mut ops, SCRATCH0, SCRATCH0, OpSize::S64);
            dynasm!(ops ; .arch aarch64 ; mov x30, x16);
            emit_address(
                &mut ops,
                STATE_REG,
                STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET as i64,
            );
            emit_store(&mut ops, 30, SCRATCH0, OpSize::S64);
        }
    }
    for block in &function.blocks {
        let label = block_labels[&block.id];
        block_offsets.push((block.id, ops.offset().0 as u64));
        dynasm!(ops
            ; .arch aarch64
            ; =>label
        );
        for instruction in &block.insts {
            emit_instruction(
                &mut ops,
                instruction,
                block.id,
                assignment,
                spill_base,
                temporary_offset,
                plan,
                &block_labels,
                &table_labels,
                function.constant_tables(),
                epilogue,
                tick_entry,
                tick_success,
                check_runtime_events,
            )?;
        }
    }

    if let Some(success) = tick_success {
        dynasm!(ops ; .arch aarch64 ; =>success ; mov x0, xzr);
    }
    dynasm!(ops
        ; .arch aarch64
        ; =>epilogue
    );
    if tick_loop {
        dynasm!(ops
            ; .arch aarch64
            ; fmov x16, d31
            ; fmov x17, d29
        );
        emit_load_imm(
            &mut ops,
            30,
            STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET as u64,
        );
        dynasm!(ops ; .arch aarch64 ; add x17, x17, x30 ; str x16, [x17]);
    }
    for &register in callee_saved.iter().rev() {
        dynasm!(ops ; .arch aarch64 ; ldr X(register), [sp], #16);
    }
    dynasm!(ops ; .arch aarch64 ; ldr x30, [sp], #16 ; ret);
    let text_size = ops.offset().0;
    for (index, table) in function.constant_tables().iter().enumerate() {
        let label = table_labels[index];
        dynasm!(ops
            ; .arch aarch64
            ; .align 8
            ; =>label
        );
        for &word in table {
            dynasm!(ops
                ; .arch aarch64
                ; .u64 word
            );
        }
    }
    let code = ops.finalize()?;
    Ok(EmitResult {
        code,
        text_size,
        frame_size: spill_frame_size,
        required_state_size,
        block_offsets,
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_instruction(
    ops: &mut VecAssembler<Aarch64Relocation>,
    instruction: &MInst,
    block: BlockId,
    assignment: &Assignment<VReg>,
    spill_base: usize,
    temporary_offset: usize,
    plan: &EdgeCopyPlan<BlockId>,
    labels: &HashMap<BlockId, DynamicLabel>,
    table_labels: &[DynamicLabel],
    constant_tables: &[Vec<u64>],
    epilogue: DynamicLabel,
    tick_entry: Option<DynamicLabel>,
    tick_success: Option<DynamicLabel>,
    check_runtime_events: bool,
) -> Result<(), EmitError> {
    match instruction {
        MInst::Mov { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            dynasm!(ops ; .arch aarch64 ; mov X(dst), X(src));
        }
        MInst::Mov32 { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            dynasm!(ops ; .arch aarch64 ; mov W(dst), W(src));
        }
        MInst::LoadImm { dst, value } => emit_load_imm(ops, resolve(assignment, *dst)?, *value),
        MInst::Scratch { .. } | MInst::KeepAlive { .. } => {}
        MInst::LoadConstantTableAddr { dst, table } => {
            let dst = resolve(assignment, *dst)?;
            let label = *table_labels
                .get(table.0)
                .ok_or(EmitError::Range("constant table identity out of range"))?;
            dynasm!(ops ; .arch aarch64 ; adr X(dst), =>label);
        }
        MInst::Load {
            dst,
            base,
            offset,
            size,
        } => {
            emit_base_address(ops, *base, *offset, spill_base)?;
            emit_load(ops, resolve(assignment, *dst)?, SCRATCH0, *size);
        }
        MInst::Store {
            base,
            offset,
            src,
            size,
        } => {
            emit_base_address(ops, *base, *offset, spill_base)?;
            emit_store(ops, resolve(assignment, *src)?, SCRATCH0, *size);
        }
        MInst::LoadPtr {
            dst,
            ptr,
            offset,
            size,
        } => {
            emit_address(ops, resolve(assignment, *ptr)?, i64::from(*offset));
            emit_load(ops, resolve(assignment, *dst)?, SCRATCH0, *size);
        }
        MInst::StorePtr {
            ptr,
            offset,
            src,
            size,
        } => {
            emit_address(ops, resolve(assignment, *ptr)?, i64::from(*offset));
            emit_store(ops, resolve(assignment, *src)?, SCRATCH0, *size);
        }
        MInst::ReleaseStorePtr {
            ptr,
            offset,
            src,
            size,
        } => {
            emit_address(ops, resolve(assignment, *ptr)?, i64::from(*offset));
            emit_release_store(ops, resolve(assignment, *src)?, SCRATCH0, *size);
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
            let base = base_register(*base);
            let index = resolve(assignment, *index)?;
            let shift = scale.trailing_zeros();
            dynasm!(ops ; .arch aarch64 ; add x16, X(base), X(index), LSL #shift);
            emit_add_offset(ops, i64::from(*offset));
            emit_load(ops, resolve(assignment, *dst)?, SCRATCH0, *size);
        }
        MInst::StoreIndexed {
            base,
            offset,
            index,
            src,
            size,
            ..
        }
        | MInst::OrStoreIndexed {
            base,
            offset,
            index,
            src,
            size,
            ..
        } => {
            let base = base_register(*base);
            let index = resolve(assignment, *index)?;
            dynasm!(ops ; .arch aarch64 ; add x16, X(base), X(index));
            emit_add_offset(ops, i64::from(*offset));
            if matches!(instruction, MInst::OrStoreIndexed { .. }) {
                emit_load(ops, SCRATCH1, SCRATCH0, *size);
                let src = resolve(assignment, *src)?;
                dynasm!(ops ; .arch aarch64 ; orr x17, x17, X(src));
                emit_store(ops, SCRATCH1, SCRATCH0, *size);
            } else {
                emit_store(ops, resolve(assignment, *src)?, SCRATCH0, *size);
            }
        }
        MInst::LoadPtrIndexed {
            dst,
            ptr,
            offset,
            index,
            size,
        } => {
            let (ptr, index) = (resolve(assignment, *ptr)?, resolve(assignment, *index)?);
            dynasm!(ops ; .arch aarch64 ; add x16, X(ptr), X(index));
            emit_add_offset(ops, i64::from(*offset));
            emit_load(ops, resolve(assignment, *dst)?, SCRATCH0, *size);
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
            let (ptr, index) = (resolve(assignment, *ptr)?, resolve(assignment, *index)?);
            dynasm!(ops ; .arch aarch64 ; add x16, X(ptr), X(index));
            emit_add_offset(ops, i64::from(*offset));
            let src = resolve(assignment, *src)?;
            if matches!(instruction, MInst::ReleaseStorePtrIndexed { .. }) {
                emit_release_store(ops, src, SCRATCH0, *size);
            } else {
                emit_store(ops, src, SCRATCH0, *size);
            }
        }
        MInst::AndStoreImm {
            base,
            offset,
            size,
            imm,
        }
        | MInst::OrStoreImm {
            base,
            offset,
            size,
            imm,
        } => {
            emit_base_address(ops, *base, *offset, spill_base)?;
            emit_load(ops, SCRATCH1, SCRATCH0, *size);
            emit_load_imm(ops, SCRATCH0, *imm);
            if matches!(instruction, MInst::AndStoreImm { .. }) {
                dynasm!(ops ; .arch aarch64 ; and x30, x17, x16);
            } else {
                dynasm!(ops ; .arch aarch64 ; orr x30, x17, x16);
            }
            emit_base_address(ops, *base, *offset, spill_base)?;
            emit_store(ops, 30, SCRATCH0, *size);
        }
        MInst::Add { dst, lhs, rhs }
        | MInst::Sub { dst, lhs, rhs }
        | MInst::Mul { dst, lhs, rhs }
        | MInst::And { dst, lhs, rhs }
        | MInst::Or { dst, lhs, rhs }
        | MInst::Xor { dst, lhs, rhs }
        | MInst::Shr { dst, lhs, rhs }
        | MInst::Shl { dst, lhs, rhs }
        | MInst::Sar { dst, lhs, rhs } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            match instruction {
                MInst::Add { .. } => dynasm!(ops ; .arch aarch64 ; add X(dst), X(lhs), X(rhs)),
                MInst::Sub { .. } => dynasm!(ops ; .arch aarch64 ; sub X(dst), X(lhs), X(rhs)),
                MInst::Mul { .. } => dynasm!(ops ; .arch aarch64 ; mul X(dst), X(lhs), X(rhs)),
                MInst::And { .. } => dynasm!(ops ; .arch aarch64 ; and X(dst), X(lhs), X(rhs)),
                MInst::Or { .. } => dynasm!(ops ; .arch aarch64 ; orr X(dst), X(lhs), X(rhs)),
                MInst::Xor { .. } => dynasm!(ops ; .arch aarch64 ; eor X(dst), X(lhs), X(rhs)),
                MInst::Shr { .. } => dynasm!(ops ; .arch aarch64 ; lsrv X(dst), X(lhs), X(rhs)),
                MInst::Shl { .. } => dynasm!(ops ; .arch aarch64 ; lslv X(dst), X(lhs), X(rhs)),
                MInst::Sar { .. } => dynasm!(ops ; .arch aarch64 ; asrv X(dst), X(lhs), X(rhs)),
                _ => unreachable!(),
            }
        }
        MInst::Add32 { dst, lhs, rhs }
        | MInst::Sub32 { dst, lhs, rhs }
        | MInst::Mul32 { dst, lhs, rhs }
        | MInst::And32 { dst, lhs, rhs }
        | MInst::Or32 { dst, lhs, rhs }
        | MInst::Xor32 { dst, lhs, rhs } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            match instruction {
                MInst::Add32 { .. } => dynasm!(ops ; .arch aarch64 ; add W(dst), W(lhs), W(rhs)),
                MInst::Sub32 { .. } => dynasm!(ops ; .arch aarch64 ; sub W(dst), W(lhs), W(rhs)),
                MInst::Mul32 { .. } => dynasm!(ops ; .arch aarch64 ; mul W(dst), W(lhs), W(rhs)),
                MInst::And32 { .. } => dynasm!(ops ; .arch aarch64 ; and W(dst), W(lhs), W(rhs)),
                MInst::Or32 { .. } => dynasm!(ops ; .arch aarch64 ; orr W(dst), W(lhs), W(rhs)),
                MInst::Xor32 { .. } => dynasm!(ops ; .arch aarch64 ; eor W(dst), W(lhs), W(rhs)),
                _ => unreachable!(),
            }
        }
        MInst::UMulHi { dst, lhs, rhs } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            dynasm!(ops ; .arch aarch64 ; umulh X(dst), X(lhs), X(rhs));
        }
        MInst::AndImm { dst, src, imm } | MInst::OrImm { dst, src, imm } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            emit_load_imm(ops, SCRATCH0, *imm);
            if matches!(instruction, MInst::AndImm { .. }) {
                dynasm!(ops ; .arch aarch64 ; and X(dst), X(src), x16);
            } else {
                dynasm!(ops ; .arch aarch64 ; orr X(dst), X(src), x16);
            }
        }
        MInst::AndImm32 { dst, src, imm } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            emit_load_imm(ops, SCRATCH0, u64::from(*imm));
            dynasm!(ops ; .arch aarch64 ; and W(dst), W(src), w16);
        }
        MInst::ShrImm { dst, src, imm }
        | MInst::ShlImm { dst, src, imm }
        | MInst::SarImm { dst, src, imm } => {
            let (dst, src, imm) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *src)?,
                u32::from(*imm),
            );
            match instruction {
                MInst::ShrImm { .. } => dynasm!(ops ; .arch aarch64 ; lsr X(dst), X(src), #imm),
                MInst::ShlImm { .. } => dynasm!(ops ; .arch aarch64 ; lsl X(dst), X(src), #imm),
                MInst::SarImm { .. } => dynasm!(ops ; .arch aarch64 ; asr X(dst), X(src), #imm),
                _ => unreachable!(),
            }
        }
        MInst::AddImm { dst, src, imm } | MInst::SubImm { dst, src, imm } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            emit_load_imm(ops, SCRATCH0, i64::from(*imm).unsigned_abs());
            let subtract = matches!(instruction, MInst::SubImm { .. }) ^ (*imm < 0);
            if subtract {
                dynasm!(ops ; .arch aarch64 ; sub X(dst), X(src), x16);
            } else {
                dynasm!(ops ; .arch aarch64 ; add X(dst), X(src), x16);
            }
        }
        MInst::Cmp {
            dst,
            lhs,
            rhs,
            kind,
        } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), X(rhs));
            emit_cset(ops, dst, *kind);
        }
        MInst::CmpImm {
            dst,
            lhs,
            imm,
            kind,
        } => {
            let (dst, lhs) = (resolve(assignment, *dst)?, resolve(assignment, *lhs)?);
            emit_load_imm(ops, SCRATCH0, *imm as i64 as u64);
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), x16);
            emit_cset(ops, dst, *kind);
        }
        MInst::UDiv { dst, lhs, rhs }
        | MInst::URem { dst, lhs, rhs }
        | MInst::SDiv { dst, lhs, rhs }
        | MInst::SRem { dst, lhs, rhs } => {
            let (dst, lhs, rhs) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
            );
            if matches!(instruction, MInst::UDiv { .. } | MInst::URem { .. }) {
                dynasm!(ops ; .arch aarch64 ; udiv x16, X(lhs), X(rhs));
            } else {
                dynasm!(ops ; .arch aarch64 ; sdiv x16, X(lhs), X(rhs));
            }
            if matches!(instruction, MInst::URem { .. } | MInst::SRem { .. }) {
                dynasm!(ops ; .arch aarch64 ; msub X(dst), x16, X(rhs), X(lhs));
            } else {
                dynasm!(ops ; .arch aarch64 ; mov X(dst), x16);
            }
        }
        MInst::BitNot { dst, src } | MInst::Neg { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            if matches!(instruction, MInst::BitNot { .. }) {
                dynasm!(ops ; .arch aarch64 ; mvn X(dst), X(src));
            } else {
                dynasm!(ops ; .arch aarch64 ; neg X(dst), X(src));
            }
        }
        MInst::Popcnt { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            dynasm!(ops
                ; .arch aarch64
                ; fmov d0, X(src)
                ; cnt v0.b8, v0.b8
                ; addv b0, v0.b8
                ; umov W(dst), v0.b[0]
            );
        }
        MInst::Bsf { dst, src } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            dynasm!(ops ; .arch aarch64 ; rbit x16, X(src) ; clz X(dst), x16);
        }
        MInst::Bsr { dst, src } | MInst::BsrOr { dst, src, .. } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            dynasm!(ops ; .arch aarch64 ; clz x16, X(src));
            emit_load_imm(ops, SCRATCH1, 63);
            dynasm!(ops ; .arch aarch64 ; sub x16, x17, x16);
            if let MInst::BsrOr { zero_value, .. } = instruction {
                emit_load_imm(ops, SCRATCH1, u64::from(*zero_value));
                dynasm!(ops ; .arch aarch64 ; cmp XSP(src), #0 ; csel X(dst), x16, x17, ne);
            } else {
                dynasm!(ops ; .arch aarch64 ; mov X(dst), x16);
            }
        }
        MInst::Select {
            dst,
            cond,
            true_val,
            false_val,
        } => {
            let (dst, cond, true_val, false_val) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *cond)?,
                resolve(assignment, *true_val)?,
                resolve(assignment, *false_val)?,
            );
            dynasm!(ops
                ; .arch aarch64
                ; cmp XSP(cond), #0
                ; csel X(dst), X(true_val), X(false_val), ne
            );
        }
        MInst::CmpSelect {
            dst,
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
        } => {
            let (dst, lhs, rhs, true_val, false_val) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
                resolve(assignment, *true_val)?,
                resolve(assignment, *false_val)?,
            );
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), X(rhs));
            emit_csel(ops, dst, true_val, false_val, *kind);
        }
        MInst::CmpImmSelect {
            dst,
            lhs,
            imm,
            kind,
            true_val,
            false_val,
        } => {
            let (dst, lhs, true_val, false_val) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *true_val)?,
                resolve(assignment, *false_val)?,
            );
            emit_load_imm(ops, SCRATCH0, *imm as i64 as u64);
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), x16);
            emit_csel(ops, dst, true_val, false_val, *kind);
        }
        MInst::JumpTable { index, targets, .. } => {
            let Some((&default_target, compared_targets)) = targets.split_last() else {
                return Err(EmitError::Range("jump table has no targets"));
            };
            let index = resolve(assignment, *index)?;
            let paths = compared_targets
                .iter()
                .map(|_| ops.new_dynamic_label())
                .collect::<Vec<_>>();
            for (table_index, path) in paths.iter().copied().enumerate() {
                emit_load_imm(ops, SCRATCH0, table_index as u64);
                dynasm!(ops ; .arch aarch64 ; cmp X(index), x16 ; b.eq =>path);
            }
            emit_edge_copies(
                ops,
                plan,
                block,
                default_target,
                spill_base,
                temporary_offset,
            )?;
            let default_label = labels[&default_target];
            dynasm!(ops ; .arch aarch64 ; b =>default_label);
            for (&target, path) in compared_targets.iter().zip(paths) {
                dynasm!(ops ; .arch aarch64 ; =>path);
                emit_edge_copies(ops, plan, block, target, spill_base, temporary_offset)?;
                let target_label = labels[&target];
                dynasm!(ops ; .arch aarch64 ; b =>target_label);
            }
        }
        MInst::Branch {
            cond,
            true_bb,
            false_bb,
        } => {
            let true_path = ops.new_dynamic_label();
            let cond = resolve(assignment, *cond)?;
            dynasm!(ops ; .arch aarch64 ; cbnz X(cond), =>true_path);
            emit_edge_copies(ops, plan, block, *false_bb, spill_base, temporary_offset)?;
            let false_label = labels[false_bb];
            dynasm!(ops ; .arch aarch64 ; b =>false_label ; =>true_path);
            emit_edge_copies(ops, plan, block, *true_bb, spill_base, temporary_offset)?;
            let true_label = labels[true_bb];
            dynasm!(ops ; .arch aarch64 ; b =>true_label);
        }
        MInst::BranchPred {
            predicate,
            true_bb,
            false_bb,
        } => {
            emit_branch_predicate(ops, *predicate, assignment, spill_base)?;
            let true_path = ops.new_dynamic_label();
            emit_conditional_branch(ops, true_path, predicate_kind(*predicate));
            emit_edge_copies(ops, plan, block, *false_bb, spill_base, temporary_offset)?;
            let false_label = labels[false_bb];
            dynasm!(ops ; .arch aarch64 ; b =>false_label ; =>true_path);
            emit_edge_copies(ops, plan, block, *true_bb, spill_base, temporary_offset)?;
            let true_label = labels[true_bb];
            dynasm!(ops ; .arch aarch64 ; b =>true_label);
        }
        MInst::Jump { target } => {
            emit_edge_copies(ops, plan, block, *target, spill_base, temporary_offset)?;
            let label = labels[target];
            dynasm!(ops ; .arch aarch64 ; b =>label);
        }
        MInst::Return => {
            if let (Some(entry), Some(success)) = (tick_entry, tick_success) {
                dynasm!(ops
                    ; .arch aarch64
                    ; fmov x16, d31
                    ; subs x16, x16, #1
                    ; fmov d31, x16
                    ; b.eq =>success
                );
                if check_runtime_events {
                    emit_address(
                        ops,
                        STATE_REG,
                        STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET as i64,
                    );
                    emit_load(ops, SCRATCH0, SCRATCH0, OpSize::S64);
                    emit_load(ops, SCRATCH0, SCRATCH0, OpSize::S64);
                    dynasm!(ops ; .arch aarch64 ; mov x30, x16);
                    emit_address(
                        ops,
                        STATE_REG,
                        STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET as i64,
                    );
                    emit_load(ops, SCRATCH1, SCRATCH0, OpSize::S64);
                    dynasm!(ops ; .arch aarch64 ; cmp x30, x17 ; b.ne =>success);
                }
                dynasm!(ops ; .arch aarch64 ; b =>entry);
            } else {
                dynasm!(ops ; .arch aarch64 ; mov x0, xzr ; b =>epilogue);
            }
        }
        MInst::ReturnError { code } => {
            if tick_entry.is_some() {
                dynasm!(ops
                    ; .arch aarch64
                    ; fmov x16, d31
                    ; sub x16, x16, #1
                    ; fmov d31, x16
                );
            }
            emit_load_imm(ops, STATE_REG, *code as u64);
            dynasm!(ops ; .arch aarch64 ; b =>epilogue);
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
        } => emit_packed_lane_compare(
            ops,
            resolve(assignment, *dst)?,
            *rhs,
            *kind,
            *offset,
            *lane_count,
            *element_stride,
            *bit_offset,
            *field_width,
            assignment,
        )?,
        MInst::PackedByteAffineCompare {
            dst,
            base,
            rhs,
            kind,
        } => emit_packed_byte_affine_compare(
            ops,
            resolve(assignment, *dst)?,
            resolve(assignment, *base)?,
            resolve(assignment, *rhs)?,
            *kind,
        ),
        MInst::MemCopy {
            src_offset,
            dst_offset,
            byte_len,
        } => emit_mem_copy(ops, *src_offset, *dst_offset, *byte_len),
        MInst::MemFill {
            dst_offset,
            byte_len,
            value,
        } => emit_mem_fill(ops, *dst_offset, *byte_len, *value),
        MInst::SparseCommit {
            src_offset,
            dst_offset,
            byte_size,
            dirty_words_offset,
            dirty_word_count,
            summary_words_offset,
            summary_word_count,
            four_state,
        } => emit_sparse_commit(
            ops,
            *src_offset,
            *dst_offset,
            *byte_size,
            *dirty_words_offset,
            *dirty_word_count,
            *summary_words_offset,
            *summary_word_count,
            *four_state,
        ),
        MInst::SparseMarkActive {
            active_index,
            active_bits_offset,
            ..
        } => {
            let word_offset = i32::try_from((*active_index as usize / 64) * 8)
                .map_err(|_| EmitError::Range("sparse active bitmap offset exceeds i32"))?;
            let offset = active_bits_offset
                .checked_add(word_offset)
                .ok_or(EmitError::Range("sparse active bitmap offset overflow"))?;
            emit_base_address(ops, BaseReg::SimState, offset, spill_base)?;
            emit_load(ops, SCRATCH1, SCRATCH0, OpSize::S64);
            emit_load_imm(ops, SCRATCH0, 1_u64 << (*active_index % 64));
            dynasm!(ops ; .arch aarch64 ; orr x30, x17, x16);
            emit_base_address(ops, BaseReg::SimState, offset, spill_base)?;
            emit_store(ops, 30, SCRATCH0, OpSize::S64);
        }
        MInst::SparseCommitWorklist {
            descriptor_table,
            active_bits_offset,
            active_capacity,
        } => emit_sparse_commit_worklist(
            ops,
            constant_tables
                .get(descriptor_table.0)
                .ok_or(EmitError::Range("sparse descriptor table is missing"))?,
            *active_bits_offset,
            *active_capacity,
        )?,
        MInst::Pext { dst, src, mask } | MInst::Pdep { dst, src, mask } => {
            emit_parallel_bits(
                ops,
                resolve(assignment, *dst)?,
                resolve(assignment, *src)?,
                resolve(assignment, *mask)?,
                matches!(instruction, MInst::Pdep { .. }),
            );
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
            let (dst, guard, lhs, rhs, true_val, false_val) = (
                resolve(assignment, *dst)?,
                resolve(assignment, *guard)?,
                resolve(assignment, *lhs)?,
                resolve(assignment, *rhs)?,
                resolve(assignment, *true_val)?,
                resolve(assignment, *false_val)?,
            );
            // Materialize the comparison before testing the guard because both
            // operations write NZCV. x16 is reserved from the allocator.
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), X(rhs));
            emit_cset(ops, SCRATCH0, *kind);
            dynasm!(ops
                ; .arch aarch64
                ; cmp XSP(guard), #0
                ; csel x16, x16, xzr, ne
                ; cmp x16, #0
                ; csel X(dst), X(true_val), X(false_val), ne
            );
        }
    }
    Ok(())
}

fn emit_mem_copy(
    ops: &mut VecAssembler<Aarch64Relocation>,
    src_offset: i32,
    dst_offset: i32,
    byte_len: usize,
) {
    if byte_len == 0 || src_offset == dst_offset {
        return;
    }
    let src_end = i64::from(src_offset) + byte_len as i64;
    let dst_end = i64::from(dst_offset) + byte_len as i64;
    let copy_backward = src_end > i64::from(dst_offset)
        && dst_end > i64::from(src_offset)
        && dst_offset > src_offset;
    let qwords = byte_len / 8;
    let remainder = byte_len % 8;

    if copy_backward {
        emit_load_imm(ops, SCRATCH0, src_end as u64);
        dynasm!(ops ; .arch aarch64 ; add x16, x0, x16);
        emit_load_imm(ops, SCRATCH1, dst_end as u64);
        dynasm!(ops ; .arch aarch64 ; add x17, x0, x17);
        if remainder >= 4 {
            dynasm!(ops
                ; .arch aarch64
                ; ldr w30, [x16, #-4]!
                ; str w30, [x17, #-4]!
            );
        }
        if remainder % 4 >= 2 {
            dynasm!(ops
                ; .arch aarch64
                ; ldrh w30, [x16, #-2]!
                ; strh w30, [x17, #-2]!
            );
        }
        if remainder % 2 == 1 {
            dynasm!(ops
                ; .arch aarch64
                ; ldrb w30, [x16, #-1]!
                ; strb w30, [x17, #-1]!
            );
        }
        if qwords != 0 {
            let loop_label = ops.new_dynamic_label();
            let done = ops.new_dynamic_label();
            emit_load_imm(ops, 30, dst_offset as i64 as u64);
            dynasm!(ops
                ; .arch aarch64
                ; add x30, x0, x30
                ; =>loop_label
                ; cmp x17, x30
                ; b.ls =>done
                ; ldr d0, [x16, #-8]!
                ; str d0, [x17, #-8]!
                ; b =>loop_label
                ; =>done
            );
        }
        return;
    }

    emit_load_imm(ops, SCRATCH0, src_offset as i64 as u64);
    dynasm!(ops ; .arch aarch64 ; add x16, x0, x16);
    emit_load_imm(ops, SCRATCH1, dst_offset as i64 as u64);
    dynasm!(ops ; .arch aarch64 ; add x17, x0, x17);
    if qwords != 0 {
        let loop_label = ops.new_dynamic_label();
        let done = ops.new_dynamic_label();
        emit_load_imm(
            ops,
            30,
            (i64::from(dst_offset) + (qwords * 8) as i64) as u64,
        );
        dynasm!(ops
            ; .arch aarch64
            ; add x30, x0, x30
            ; =>loop_label
            ; cmp x17, x30
            ; b.hs =>done
            ; ldr d0, [x16], #8
            ; str d0, [x17], #8
            ; b =>loop_label
            ; =>done
        );
    }
    if remainder >= 4 {
        dynasm!(ops ; .arch aarch64 ; ldr w30, [x16], #4 ; str w30, [x17], #4);
    }
    if remainder % 4 >= 2 {
        dynasm!(ops ; .arch aarch64 ; ldrh w30, [x16], #2 ; strh w30, [x17], #2);
    }
    if remainder % 2 == 1 {
        dynasm!(ops ; .arch aarch64 ; ldrb w30, [x16] ; strb w30, [x17]);
    }
}

fn emit_parallel_bits(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    source: u8,
    mask: u8,
    deposit: bool,
) {
    // Save both inputs before defining the result: allocation may coalesce a
    // dying input with the destination. The fixed unroll avoids a hidden GPR
    // clobber while matching BMI2 semantics on baseline ARMv8-A.
    dynasm!(ops
        ; .arch aarch64
        ; fmov d6, X(source)
        ; fmov d7, X(mask)
        ; mov x17, xzr
        ; mov x30, xzr
    );
    for bit in 0..64_u32 {
        let skip = ops.new_dynamic_label();
        dynasm!(ops ; .arch aarch64 ; fmov x16, d7 ; tbz x16, bit, =>skip ; fmov x16, d6);
        if deposit {
            dynasm!(ops ; .arch aarch64 ; lsr x16, x16, x17 ; and x16, x16, #1);
            if bit != 0 {
                dynasm!(ops ; .arch aarch64 ; lsl x16, x16, bit);
            }
        } else {
            if bit != 0 {
                dynasm!(ops ; .arch aarch64 ; lsr x16, x16, bit);
            }
            dynasm!(ops ; .arch aarch64 ; and x16, x16, #1 ; lsl x16, x16, x17);
        }
        dynasm!(ops
            ; .arch aarch64
            ; orr x30, x30, x16
            ; add x17, x17, #1
            ; =>skip
        );
    }
    dynasm!(ops ; .arch aarch64 ; mov X(destination), x30);
}

#[allow(clippy::too_many_arguments)]
fn emit_packed_lane_compare(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    rhs: PackedLaneCompareRhs,
    kind: CmpKind,
    offset: i32,
    lane_count: u8,
    element_stride: u8,
    bit_offset: u8,
    field_width: u8,
    assignment: &Assignment<VReg>,
) -> Result<(), EmitError> {
    let size = match element_stride {
        1 => OpSize::S8,
        2 => OpSize::S16,
        4 => OpSize::S32,
        _ => return Err(EmitError::Range("packed lane stride must be 1, 2, or 4")),
    };
    if let PackedLaneCompareRhs::Scalar(value) = rhs {
        let register = resolve(assignment, value)?;
        dynasm!(ops ; .arch aarch64 ; fmov d6, X(register));
    }
    dynasm!(ops ; .arch aarch64 ; mov x30, xzr);
    let field_mask = if field_width == 64 {
        u64::MAX
    } else {
        (1_u64 << field_width) - 1
    };
    for lane in 0..lane_count {
        let lane_delta = i32::from(lane)
            .checked_mul(i32::from(element_stride))
            .and_then(|delta| offset.checked_add(delta))
            .ok_or(EmitError::Range("packed lane offset overflow"))?;
        emit_address(ops, STATE_REG, i64::from(lane_delta));
        emit_load(ops, SCRATCH0, SCRATCH0, size);
        match rhs {
            PackedLaneCompareRhs::Scalar(_) => {
                dynasm!(ops ; .arch aarch64 ; fmov x17, d6);
            }
            PackedLaneCompareRhs::Memory { offset, .. } => {
                dynasm!(ops ; .arch aarch64 ; fmov d7, x16);
                let rhs_offset = i32::from(lane)
                    .checked_mul(i32::from(element_stride))
                    .and_then(|delta| offset.checked_add(delta))
                    .ok_or(EmitError::Range("packed lane RHS offset overflow"))?;
                emit_address(ops, STATE_REG, i64::from(rhs_offset));
                emit_load(ops, SCRATCH1, SCRATCH0, size);
                dynasm!(ops ; .arch aarch64 ; fmov x16, d7);
            }
        }
        if bit_offset != 0 {
            let shift = u32::from(bit_offset);
            dynasm!(ops
                ; .arch aarch64
                ; lsr x16, x16, shift
                ; lsr x17, x17, shift
            );
        }
        let storage_width = element_stride * 8;
        if field_width != storage_width {
            dynasm!(ops ; .arch aarch64 ; fmov d5, x30);
            emit_load_imm(ops, 30, field_mask);
            dynasm!(ops
                ; .arch aarch64
                ; and x16, x16, x30
                ; and x17, x17, x30
                ; fmov x30, d5
            );
        }
        if matches!(
            kind,
            CmpKind::LtS | CmpKind::LeS | CmpKind::GtS | CmpKind::GeS
        ) {
            match storage_width {
                8 => dynasm!(ops ; .arch aarch64 ; sxtb x16, w16 ; sxtb x17, w17),
                16 => dynasm!(ops ; .arch aarch64 ; sxth x16, w16 ; sxth x17, w17),
                32 => dynasm!(ops ; .arch aarch64 ; sxtw x16, w16 ; sxtw x17, w17),
                _ => unreachable!(),
            }
        }
        dynasm!(ops ; .arch aarch64 ; cmp x16, x17);
        emit_cset(ops, SCRATCH0, kind);
        if lane != 0 {
            let shift = u32::from(lane);
            dynasm!(ops ; .arch aarch64 ; lsl x16, x16, shift);
        }
        dynasm!(ops ; .arch aarch64 ; orr x30, x30, x16);
    }
    dynasm!(ops ; .arch aarch64 ; mov X(destination), x30);
    Ok(())
}

fn emit_packed_byte_affine_compare(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    rhs: u8,
    kind: CmpKind,
) {
    dynasm!(ops
        ; .arch aarch64
        ; fmov d6, X(base)
        ; fmov d7, X(rhs)
        ; mov x30, xzr
    );
    for lane in 0..16_u32 {
        dynasm!(ops ; .arch aarch64 ; fmov x16, d6);
        if lane != 0 {
            emit_load_imm(ops, SCRATCH1, u64::from(lane));
            dynasm!(ops ; .arch aarch64 ; add x16, x16, x17);
        }
        dynasm!(ops ; .arch aarch64 ; fmov x17, d7 ; fmov d5, x30);
        emit_load_imm(ops, 30, 0xff);
        dynasm!(ops
            ; .arch aarch64
            ; and x16, x16, x30
            ; and x17, x17, x30
            ; fmov x30, d5
        );
        if matches!(
            kind,
            CmpKind::LtS | CmpKind::LeS | CmpKind::GtS | CmpKind::GeS
        ) {
            dynasm!(ops ; .arch aarch64 ; sxtb x16, w16 ; sxtb x17, w17);
        }
        dynasm!(ops ; .arch aarch64 ; cmp x16, x17);
        emit_cset(ops, SCRATCH0, kind);
        if lane != 0 {
            dynasm!(ops ; .arch aarch64 ; lsl x16, x16, lane);
        }
        dynasm!(ops ; .arch aarch64 ; orr x30, x30, x16);
    }
    dynasm!(ops ; .arch aarch64 ; mov X(destination), x30);
}

fn emit_sparse_commit_worklist(
    ops: &mut VecAssembler<Aarch64Relocation>,
    descriptors: &[u64],
    active_bits_offset: i32,
    active_capacity: usize,
) -> Result<(), EmitError> {
    for word_index in 0..active_capacity.div_ceil(64) {
        let word_offset = i32::try_from(word_index * 8)
            .map_err(|_| EmitError::Range("sparse active bitmap offset exceeds i32"))?;
        let offset = active_bits_offset
            .checked_add(word_offset)
            .ok_or(EmitError::Range("sparse active bitmap offset overflow"))?;
        let word_done = ops.new_dynamic_label();
        emit_address(ops, STATE_REG, i64::from(offset));
        dynasm!(ops
            ; .arch aarch64
            ; ldr x17, [x16]
            ; str xzr, [x16]
            ; cbz x17, =>word_done
        );
        let first_index = word_index * 64;
        let end_index = active_capacity.min(first_index + 64);
        for active_index in first_index..end_index {
            let row_start = active_index
                .checked_mul(SPARSE_COMMIT_DESCRIPTOR_WORDS)
                .ok_or(EmitError::Range("sparse descriptor index overflow"))?;
            let row = descriptors
                .get(row_start..row_start + SPARSE_COMMIT_DESCRIPTOR_WORDS)
                .ok_or(EmitError::Range("sparse descriptor row is missing"))?;
            let skip = ops.new_dynamic_label();
            let mask = 1_u64 << (active_index % 64);
            emit_load_imm(ops, SCRATCH0, mask);
            dynasm!(ops
                ; .arch aarch64
                ; and x16, x17, x16
                ; cbz x16, =>skip
                ; fmov d5, x17
            );
            emit_sparse_commit(
                ops,
                i32::try_from(row[0])
                    .map_err(|_| EmitError::Range("sparse source offset exceeds i32"))?,
                i32::try_from(row[1])
                    .map_err(|_| EmitError::Range("sparse destination offset exceeds i32"))?,
                usize::try_from(row[2])
                    .map_err(|_| EmitError::Range("sparse byte size exceeds usize"))?,
                i32::try_from(row[3])
                    .map_err(|_| EmitError::Range("sparse dirty offset exceeds i32"))?,
                usize::try_from(row[4])
                    .map_err(|_| EmitError::Range("sparse dirty count exceeds usize"))?,
                i32::try_from(row[5])
                    .map_err(|_| EmitError::Range("sparse summary offset exceeds i32"))?,
                usize::try_from(row[6])
                    .map_err(|_| EmitError::Range("sparse summary count exceeds usize"))?,
                row[7] != 0,
            );
            dynasm!(ops ; .arch aarch64 ; fmov x17, d5 ; =>skip);
        }
        dynasm!(ops ; .arch aarch64 ; =>word_done);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_sparse_commit(
    ops: &mut VecAssembler<Aarch64Relocation>,
    src_offset: i32,
    dst_offset: i32,
    byte_size: usize,
    dirty_words_offset: i32,
    dirty_word_count: usize,
    summary_words_offset: i32,
    summary_word_count: usize,
    four_state: bool,
) {
    let chunk_count = byte_size.div_ceil(8);
    let last_chunk = chunk_count.saturating_sub(1);
    let last_len = byte_size.saturating_sub(last_chunk * 8);
    let plane_count = if four_state { 2 } else { 1 };

    for summary_index in 0..summary_word_count {
        let summary_loop = ops.new_dynamic_label();
        let summary_done = ops.new_dynamic_label();
        let summary_next = ops.new_dynamic_label();
        let dirty_loop = ops.new_dynamic_label();
        let dirty_restore = ops.new_dynamic_label();
        let summary_offset = i64::from(summary_words_offset) + (summary_index * 8) as i64;
        emit_address(ops, STATE_REG, summary_offset);
        dynasm!(ops
            ; .arch aarch64
            ; ldr x17, [x16]
            ; str xzr, [x16]
            ; mov x16, x17
            ; =>summary_loop
            ; cbz x16, =>summary_done
            ; rbit x17, x16
            ; clz x17, x17
            ; sub x30, x16, #1
            ; and x16, x16, x30
        );
        if summary_index != 0 {
            emit_load_imm(ops, 30, (summary_index * 64) as u64);
            dynasm!(ops ; .arch aarch64 ; add x17, x17, x30);
        }
        emit_load_imm(ops, 30, dirty_word_count as u64);
        dynasm!(ops
            ; .arch aarch64
            ; cmp x17, x30
            ; b.hs =>summary_loop
            ; fmov d0, x16
            ; fmov d1, x17
            ; lsl x17, x17, #3
        );
        emit_load_imm(ops, SCRATCH0, dirty_words_offset as i64 as u64);
        dynasm!(ops
            ; .arch aarch64
            ; add x16, x0, x16
            ; add x16, x16, x17
            ; ldr x30, [x16]
            ; str xzr, [x16]
            ; =>dirty_loop
            ; cbz x30, =>summary_next
            ; rbit x17, x30
            ; clz x17, x17
            ; sub x16, x30, #1
            ; and x30, x30, x16
            ; fmov d2, x30
            ; fmov x16, d1
            ; lsl x16, x16, #6
            ; add x17, x16, x17
        );
        emit_load_imm(ops, SCRATCH0, chunk_count as u64);
        dynasm!(ops
            ; .arch aarch64
            ; cmp x17, x16
            ; b.hs =>dirty_restore
            ; lsl x17, x17, #3
            ; fmov d4, x17
        );

        if last_len != 8 {
            let full_chunk = ops.new_dynamic_label();
            let copy_done = ops.new_dynamic_label();
            emit_load_imm(ops, SCRATCH0, (last_chunk * 8) as u64);
            dynasm!(ops ; .arch aarch64 ; cmp x17, x16 ; b.ne =>full_chunk);
            for plane in 0..plane_count {
                let delta = (plane * byte_size) as i32;
                emit_sparse_chunk_copy(ops, src_offset + delta, dst_offset + delta, last_len);
            }
            dynasm!(ops ; .arch aarch64 ; b =>copy_done ; =>full_chunk);
            for plane in 0..plane_count {
                let delta = (plane * byte_size) as i32;
                emit_sparse_chunk_copy(ops, src_offset + delta, dst_offset + delta, 8);
            }
            dynasm!(ops ; .arch aarch64 ; =>copy_done);
        } else {
            for plane in 0..plane_count {
                let delta = (plane * byte_size) as i32;
                emit_sparse_chunk_copy(ops, src_offset + delta, dst_offset + delta, 8);
            }
        }
        dynasm!(ops
            ; .arch aarch64
            ; =>dirty_restore
            ; fmov x30, d2
            ; b =>dirty_loop
            ; =>summary_next
            ; fmov x16, d0
            ; b =>summary_loop
            ; =>summary_done
        );
    }
}

fn emit_sparse_chunk_copy(
    ops: &mut VecAssembler<Aarch64Relocation>,
    src_offset: i32,
    dst_offset: i32,
    byte_len: usize,
) {
    dynasm!(ops ; .arch aarch64 ; fmov x17, d4);
    emit_load_imm(ops, SCRATCH0, src_offset as i64 as u64);
    dynasm!(ops ; .arch aarch64 ; add x16, x0, x16 ; add x16, x16, x17);
    emit_load_imm(ops, 30, dst_offset as i64 as u64);
    dynasm!(ops ; .arch aarch64 ; add x30, x0, x30 ; add x30, x30, x17);
    if byte_len == 8 {
        dynasm!(ops ; .arch aarch64 ; ldr d3, [x16] ; str d3, [x30]);
        return;
    }
    if byte_len >= 4 {
        dynasm!(ops ; .arch aarch64 ; ldr w17, [x16], #4 ; str w17, [x30], #4);
    }
    if byte_len % 4 >= 2 {
        dynasm!(ops ; .arch aarch64 ; ldrh w17, [x16], #2 ; strh w17, [x30], #2);
    }
    if byte_len % 2 == 1 {
        dynasm!(ops ; .arch aarch64 ; ldrb w17, [x16] ; strb w17, [x30]);
    }
}

fn emit_mem_fill(
    ops: &mut VecAssembler<Aarch64Relocation>,
    dst_offset: i32,
    byte_len: usize,
    value: u8,
) {
    if byte_len == 0 {
        return;
    }
    let qwords = byte_len / 8;
    let remainder = byte_len % 8;
    let pattern = u64::from(value) * 0x0101_0101_0101_0101;
    emit_load_imm(ops, SCRATCH0, dst_offset as i64 as u64);
    dynasm!(ops ; .arch aarch64 ; add x16, x0, x16);
    emit_load_imm(ops, 30, pattern);
    if qwords != 0 {
        let loop_label = ops.new_dynamic_label();
        let done = ops.new_dynamic_label();
        emit_load_imm(
            ops,
            SCRATCH1,
            (i64::from(dst_offset) + (qwords * 8) as i64) as u64,
        );
        dynasm!(ops
            ; .arch aarch64
            ; add x17, x0, x17
            ; =>loop_label
            ; cmp x16, x17
            ; b.hs =>done
            ; str x30, [x16], #8
            ; b =>loop_label
            ; =>done
        );
    }
    if remainder >= 4 {
        dynasm!(ops ; .arch aarch64 ; str w30, [x16], #4);
    }
    if remainder % 4 >= 2 {
        dynasm!(ops ; .arch aarch64 ; strh w30, [x16], #2);
    }
    if remainder % 2 == 1 {
        dynasm!(ops ; .arch aarch64 ; strb w30, [x16]);
    }
}

fn base_register(base: BaseReg) -> u8 {
    match base {
        BaseReg::SimState => STATE_REG,
        BaseReg::StackFrame => STATE_REG,
    }
}

fn emit_base_address(
    ops: &mut VecAssembler<Aarch64Relocation>,
    base: BaseReg,
    offset: i32,
    spill_base: usize,
) -> Result<(), EmitError> {
    let offset = match base {
        BaseReg::SimState => i64::from(offset),
        BaseReg::StackFrame => i64::try_from(spill_base)
            .ok()
            .and_then(|base| base.checked_add(i64::from(offset)))
            .ok_or(EmitError::Range("stack-frame address overflow"))?,
    };
    emit_address(ops, STATE_REG, offset);
    Ok(())
}

fn emit_address(ops: &mut VecAssembler<Aarch64Relocation>, base: u8, offset: i64) {
    if offset == 0 {
        dynasm!(ops ; .arch aarch64 ; mov x16, X(base));
    } else {
        emit_load_imm(ops, SCRATCH1, offset as u64);
        dynasm!(ops ; .arch aarch64 ; add x16, X(base), x17);
    }
}

fn emit_add_offset(ops: &mut VecAssembler<Aarch64Relocation>, offset: i64) {
    if offset != 0 {
        emit_load_imm(ops, SCRATCH1, offset as u64);
        dynasm!(ops ; .arch aarch64 ; add x16, x16, x17);
    }
}

fn emit_load_imm(ops: &mut VecAssembler<Aarch64Relocation>, register: u8, value: u64) {
    let halves = [
        value as u16,
        (value >> 16) as u16,
        (value >> 32) as u16,
        (value >> 48) as u16,
    ];
    let first = halves.iter().position(|half| *half != 0).unwrap_or(0);
    let half = u32::from(halves[first]);
    match first {
        0 => dynasm!(ops ; .arch aarch64 ; movz X(register), half),
        1 => dynasm!(ops ; .arch aarch64 ; movz X(register), half, LSL #16),
        2 => dynasm!(ops ; .arch aarch64 ; movz X(register), half, LSL #32),
        3 => dynasm!(ops ; .arch aarch64 ; movz X(register), half, LSL #48),
        _ => unreachable!(),
    }
    for (index, half) in halves.into_iter().enumerate() {
        if index == first || half == 0 {
            continue;
        }
        let half = u32::from(half);
        match index {
            0 => dynasm!(ops ; .arch aarch64 ; movk X(register), half),
            1 => dynasm!(ops ; .arch aarch64 ; movk X(register), half, LSL #16),
            2 => dynasm!(ops ; .arch aarch64 ; movk X(register), half, LSL #32),
            3 => dynasm!(ops ; .arch aarch64 ; movk X(register), half, LSL #48),
            _ => unreachable!(),
        }
    }
}

fn emit_load(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    address: u8,
    size: OpSize,
) {
    match size {
        OpSize::S8 => dynasm!(ops ; .arch aarch64 ; ldrb W(destination), [X(address)]),
        OpSize::S16 => dynasm!(ops ; .arch aarch64 ; ldrh W(destination), [X(address)]),
        OpSize::S32 => dynasm!(ops ; .arch aarch64 ; ldr W(destination), [X(address)]),
        OpSize::S64 => dynasm!(ops ; .arch aarch64 ; ldr X(destination), [X(address)]),
    }
}

fn emit_store(ops: &mut VecAssembler<Aarch64Relocation>, source: u8, address: u8, size: OpSize) {
    match size {
        OpSize::S8 => dynasm!(ops ; .arch aarch64 ; strb W(source), [X(address)]),
        OpSize::S16 => dynasm!(ops ; .arch aarch64 ; strh W(source), [X(address)]),
        OpSize::S32 => dynasm!(ops ; .arch aarch64 ; str W(source), [X(address)]),
        OpSize::S64 => dynasm!(ops ; .arch aarch64 ; str X(source), [X(address)]),
    }
}

fn emit_release_store(
    ops: &mut VecAssembler<Aarch64Relocation>,
    source: u8,
    address: u8,
    size: OpSize,
) {
    match size {
        OpSize::S8 => dynasm!(ops ; .arch aarch64 ; stlrb W(source), [X(address)]),
        OpSize::S16 => dynasm!(ops ; .arch aarch64 ; stlrh W(source), [X(address)]),
        OpSize::S32 => dynasm!(ops ; .arch aarch64 ; stlr W(source), [X(address)]),
        OpSize::S64 => dynasm!(ops ; .arch aarch64 ; stlr X(source), [X(address)]),
    }
}

fn emit_cset(ops: &mut VecAssembler<Aarch64Relocation>, destination: u8, kind: CmpKind) {
    match kind {
        CmpKind::Eq => dynasm!(ops ; .arch aarch64 ; cset X(destination), eq),
        CmpKind::Ne => dynasm!(ops ; .arch aarch64 ; cset X(destination), ne),
        CmpKind::LtU => dynasm!(ops ; .arch aarch64 ; cset X(destination), lo),
        CmpKind::LtS => dynasm!(ops ; .arch aarch64 ; cset X(destination), lt),
        CmpKind::LeU => dynasm!(ops ; .arch aarch64 ; cset X(destination), ls),
        CmpKind::LeS => dynasm!(ops ; .arch aarch64 ; cset X(destination), le),
        CmpKind::GtU => dynasm!(ops ; .arch aarch64 ; cset X(destination), hi),
        CmpKind::GtS => dynasm!(ops ; .arch aarch64 ; cset X(destination), gt),
        CmpKind::GeU => dynasm!(ops ; .arch aarch64 ; cset X(destination), hs),
        CmpKind::GeS => dynasm!(ops ; .arch aarch64 ; cset X(destination), ge),
    }
}

fn emit_csel(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    true_value: u8,
    false_value: u8,
    kind: CmpKind,
) {
    match kind {
        CmpKind::Eq => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), eq)
        }
        CmpKind::Ne => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), ne)
        }
        CmpKind::LtU => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), lo)
        }
        CmpKind::LtS => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), lt)
        }
        CmpKind::LeU => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), ls)
        }
        CmpKind::LeS => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), le)
        }
        CmpKind::GtU => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), hi)
        }
        CmpKind::GtS => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), gt)
        }
        CmpKind::GeU => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), hs)
        }
        CmpKind::GeS => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), ge)
        }
    }
}

fn emit_branch_predicate(
    ops: &mut VecAssembler<Aarch64Relocation>,
    predicate: BranchPredicate,
    assignment: &Assignment<VReg>,
    spill_base: usize,
) -> Result<(), EmitError> {
    match predicate {
        BranchPredicate::Compare { lhs, rhs, .. } => {
            let (lhs, rhs) = (resolve(assignment, lhs)?, resolve(assignment, rhs)?);
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), X(rhs));
        }
        BranchPredicate::CompareImm { lhs, imm, .. } => {
            let lhs = resolve(assignment, lhs)?;
            emit_load_imm(ops, SCRATCH0, imm as i64 as u64);
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), x16);
        }
        BranchPredicate::MemoryNonZero { base, offset, size } => {
            emit_base_address(ops, base, offset, spill_base)?;
            emit_load(ops, SCRATCH0, SCRATCH0, size);
            dynasm!(ops ; .arch aarch64 ; cmp x16, #0);
        }
    }
    Ok(())
}

fn predicate_kind(predicate: BranchPredicate) -> CmpKind {
    match predicate {
        BranchPredicate::Compare { kind, .. } | BranchPredicate::CompareImm { kind, .. } => kind,
        BranchPredicate::MemoryNonZero { .. } => CmpKind::Ne,
    }
}

fn emit_conditional_branch(
    ops: &mut VecAssembler<Aarch64Relocation>,
    label: DynamicLabel,
    kind: CmpKind,
) {
    match kind {
        CmpKind::Eq => dynasm!(ops ; .arch aarch64 ; b.eq =>label),
        CmpKind::Ne => dynasm!(ops ; .arch aarch64 ; b.ne =>label),
        CmpKind::LtU => dynasm!(ops ; .arch aarch64 ; b.lo =>label),
        CmpKind::LtS => dynasm!(ops ; .arch aarch64 ; b.lt =>label),
        CmpKind::LeU => dynasm!(ops ; .arch aarch64 ; b.ls =>label),
        CmpKind::LeS => dynasm!(ops ; .arch aarch64 ; b.le =>label),
        CmpKind::GtU => dynasm!(ops ; .arch aarch64 ; b.hi =>label),
        CmpKind::GtS => dynasm!(ops ; .arch aarch64 ; b.gt =>label),
        CmpKind::GeU => dynasm!(ops ; .arch aarch64 ; b.hs =>label),
        CmpKind::GeS => dynasm!(ops ; .arch aarch64 ; b.ge =>label),
    }
}

fn emit_edge_copies(
    ops: &mut VecAssembler<Aarch64Relocation>,
    plan: &EdgeCopyPlan<BlockId>,
    predecessor: BlockId,
    successor: BlockId,
    spill_base: usize,
    temporary_offset: usize,
) -> Result<(), EmitError> {
    let Some(operations) = plan.edge(predecessor, successor) else {
        return Ok(());
    };
    for operation in operations {
        match *operation {
            CopyOperation::Move {
                destination,
                source,
            } => emit_copy(ops, destination, source, spill_base)?,
            CopyOperation::SwapRegisters { left, right } => {
                let (left, right) = (left.number(), right.number());
                dynasm!(ops
                    ; .arch aarch64
                    ; mov x16, X(left)
                    ; mov X(left), X(right)
                    ; mov X(right), x16
                );
            }
            CopyOperation::SaveTemporary(destination) => {
                read_copy_destination(ops, destination, spill_base, SCRATCH1)?;
                // Address materialization uses x17 for the offset. Preserve
                // the value being saved before computing the temporary slot.
                dynasm!(ops ; .arch aarch64 ; mov x30, x17);
                emit_address(ops, STATE_REG, temporary_offset as i64);
                emit_store(ops, 30, SCRATCH0, OpSize::S64);
            }
            CopyOperation::RestoreTemporary(destination) => {
                emit_address(ops, STATE_REG, temporary_offset as i64);
                emit_load(ops, SCRATCH1, SCRATCH0, OpSize::S64);
                write_copy_destination(ops, destination, spill_base, SCRATCH1)?;
            }
        }
    }
    Ok(())
}

fn emit_copy(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: CopyDestination,
    source: CopySource,
    spill_base: usize,
) -> Result<(), EmitError> {
    match source {
        CopySource::Register(register) => {
            write_copy_destination(ops, destination, spill_base, register.number())
        }
        CopySource::Stack(offset) => {
            emit_address(ops, STATE_REG, spill_base as i64 + i64::from(offset));
            emit_load(ops, SCRATCH1, SCRATCH0, OpSize::S64);
            write_copy_destination(ops, destination, spill_base, SCRATCH1)
        }
        CopySource::Immediate(value) => {
            emit_load_imm(ops, SCRATCH1, value);
            write_copy_destination(ops, destination, spill_base, SCRATCH1)
        }
    }
}

fn read_copy_destination(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: CopyDestination,
    spill_base: usize,
    output: u8,
) -> Result<(), EmitError> {
    match destination {
        CopyDestination::Register(register) => {
            let register = register.number();
            dynasm!(ops ; .arch aarch64 ; mov X(output), X(register));
        }
        CopyDestination::Stack(offset) => {
            emit_address(ops, STATE_REG, spill_base as i64 + i64::from(offset));
            emit_load(ops, output, SCRATCH0, OpSize::S64);
        }
    }
    Ok(())
}

fn write_copy_destination(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: CopyDestination,
    spill_base: usize,
    source: u8,
) -> Result<(), EmitError> {
    match destination {
        CopyDestination::Register(register) => {
            let register = register.number();
            dynasm!(ops ; .arch aarch64 ; mov X(register), X(source));
        }
        CopyDestination::Stack(offset) => {
            let source = if source == SCRATCH1 {
                // emit_address reserves x17 for large/immediate offsets.
                // Stack-to-stack and temporary restores arrive in x17, so
                // retain their payload in the other fixed scratch register.
                dynasm!(ops ; .arch aarch64 ; mov x30, x17);
                30
            } else {
                source
            };
            emit_address(ops, STATE_REG, spill_base as i64 + i64::from(offset));
            emit_store(ops, source, SCRATCH0, OpSize::S64);
        }
    }
    Ok(())
}

/// Hex-word fallback used in traces until an AArch64 disassembler is added.
pub fn disassemble(code: &[u8], base_addr: u64) -> String {
    code.chunks_exact(4)
        .enumerate()
        .map(|(index, bytes)| {
            let word = u32::from_le_bytes(bytes.try_into().expect("four-byte chunk"));
            format!("{:08x}: {word:08x}\n", base_addr + (index * 4) as u64)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_arch = "aarch64")]
    use crate::mir as arm_mir;
    use celox_backend_x86::native::mir::{
        BaseReg, BlockId, MBlock, MFunction, MInst, OpSize, SpillDesc, VRegAllocator,
    };
    #[cfg(target_arch = "aarch64")]
    use celox_backend_x86::native::mir::{
        CmpKind, PackedLaneCompareRhs, PhiNode, SparseCommitDescriptor,
    };
    use celox_backend_x86::native::regalloc::AssignmentMap;
    #[cfg(target_arch = "aarch64")]
    use celox_backend_x86::native::regalloc::assignment::EdgeLocation;
    use celox_backend_x86::native::regalloc::assignment::PhysReg;

    fn state_update() -> (MFunction, AssignmentMap) {
        let mut vregs = VRegAllocator::new();
        let loaded = vregs.alloc();
        let increment = vregs.alloc();
        let result = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: loaded,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: increment,
            value: 5,
        });
        block.push(MInst::Add {
            dst: result,
            lhs: loaded,
            rhs: increment,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: result,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        function.push_block(block);
        let mut assignment = AssignmentMap::default();
        assignment.set(loaded, PhysReg::RAX);
        assignment.set(increment, PhysReg::RDX);
        assignment.set(result, PhysReg::RSI);
        (function, assignment)
    }

    #[cfg(target_arch = "aarch64")]
    fn guarded_select() -> (MFunction, AssignmentMap) {
        let mut vregs = VRegAllocator::new();
        let guard = vregs.alloc();
        let lhs = vregs.alloc();
        let rhs = vregs.alloc();
        let true_value = vregs.alloc();
        let false_value = vregs.alloc();
        let result = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
        let mut block = MBlock::new(BlockId(0));
        for (dst, offset) in [(guard, 0), (lhs, 8), (rhs, 16)] {
            block.push(MInst::Load {
                dst,
                base: BaseReg::SimState,
                offset,
                size: OpSize::S64,
            });
        }
        block.push(MInst::LoadImm {
            dst: true_value,
            value: 0xaa,
        });
        block.push(MInst::LoadImm {
            dst: false_value,
            value: 0x55,
        });
        block.push(MInst::GuardedCmpSelect {
            dst: result,
            guard,
            lhs,
            rhs,
            kind: CmpKind::LtU,
            true_val: true_value,
            false_val: false_value,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 24,
            src: result,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        function.push_block(block);
        let mut assignment = AssignmentMap::default();
        for (value, register) in [
            (guard, PhysReg::RAX),
            (lhs, PhysReg::RDX),
            (rhs, PhysReg::RSI),
            (true_value, PhysReg::RDI),
            (false_value, PhysReg::R8),
            (result, PhysReg::R9),
        ] {
            assignment.set(value, register);
        }
        (function, assignment)
    }

    #[cfg(target_arch = "aarch64")]
    fn bulk_memory_ops() -> (MFunction, AssignmentMap) {
        let mut function = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::MemCopy {
            src_offset: 0,
            dst_offset: 4,
            byte_len: 12,
        });
        block.push(MInst::MemFill {
            dst_offset: 20,
            byte_len: 13,
            value: 0xa5,
        });
        block.push(MInst::Return);
        function.push_block(block);
        (function, AssignmentMap::default())
    }

    #[test]
    fn emits_allocated_scalar_mir() {
        let (function, assignment) = state_update();
        let result = emit(&function, &assignment, 0).unwrap();
        assert!(!result.code.is_empty());
        assert!(result.text_size <= result.code.len());
        assert_eq!(result.block_offsets, vec![(crate::mir::BlockId(0), 4)]);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_allocated_scalar_mir() {
        let (function, assignment) = state_update();
        let result = emit(&function, &assignment, 0).unwrap();
        let code = crate::jit_mem::JitCode::new(&result.code).unwrap();
        let mut state = vec![0_u8; 16];
        state[..8].copy_from_slice(&37_u64.to_le_bytes());
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        assert_eq!(u64::from_le_bytes(state[8..].try_into().unwrap()), 42);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_target_owned_spills_and_reloads() {
        let mut instructions = (0..26)
            .map(|value| arm_mir::MInst::LoadImm {
                dst: arm_mir::VReg(value),
                value: u64::from(value) + 100,
            })
            .collect::<Vec<_>>();
        instructions.extend((0..26).map(|value| arm_mir::MInst::Store {
            base: arm_mir::BaseReg::SimState,
            offset: value * 8,
            src: arm_mir::VReg(value as u32),
            size: arm_mir::OpSize::S64,
        }));
        instructions.push(arm_mir::MInst::Return);
        let allocation = crate::regalloc::allocate_with_spills(arm_mir::MFunction::new(
            vec![arm_mir::MBlock {
                id: arm_mir::BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
            Vec::new(),
        ))
        .unwrap();
        assert!(allocation.spill_frame_size > 0);
        let emitted = emit_function(
            &allocation.allocated.function,
            &allocation.allocated.assignment,
            allocation.spill_frame_size,
            26 * 8,
            &allocation.allocated.edge_copies,
            false,
            false,
        )
        .unwrap();
        let code = crate::jit_mem::JitCode::new(&emitted.code).unwrap();
        let mut state = vec![0_u8; 512];

        assert_eq!(unsafe { code.call(&mut state) }, 0);
        for value in 0..26_usize {
            assert_eq!(
                u64::from_le_bytes(state[value * 8..value * 8 + 8].try_into().unwrap()),
                value as u64 + 100
            );
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_guarded_compare_select() {
        let (function, assignment) = guarded_select();
        let result = emit(&function, &assignment, 0).unwrap();
        let code = crate::jit_mem::JitCode::new(&result.code).unwrap();
        let mut state = vec![0_u8; 32];
        state[..8].copy_from_slice(&1_u64.to_le_bytes());
        state[8..16].copy_from_slice(&3_u64.to_le_bytes());
        state[16..24].copy_from_slice(&7_u64.to_le_bytes());
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        assert_eq!(u64::from_le_bytes(state[24..].try_into().unwrap()), 0xaa);

        state[..8].copy_from_slice(&0_u64.to_le_bytes());
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        assert_eq!(u64::from_le_bytes(state[24..].try_into().unwrap()), 0x55);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_overlap_copy_and_fill() {
        let (function, assignment) = bulk_memory_ops();
        let result = emit(&function, &assignment, 0).unwrap();
        let code = crate::jit_mem::JitCode::new(&result.code).unwrap();
        let mut state = vec![0_u8; 40];
        for (index, byte) in state[..16].iter_mut().enumerate() {
            *byte = index as u8;
        }
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        assert_eq!(&state[4..16], &(0_u8..12).collect::<Vec<_>>());
        assert_eq!(&state[20..33], &[0xa5; 13]);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_every_jump_table_target() {
        let mut vregs = VRegAllocator::new();
        let loaded = vregs.alloc();
        let index = vregs.alloc();
        let table_base = vregs.alloc();
        let target = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
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
        function.push_block(entry);
        for code in 1..=4 {
            let mut arm = MBlock::new(BlockId(code));
            arm.push(MInst::ReturnError {
                code: i64::from(code),
            });
            function.push_block(arm);
        }
        let mut assignment = AssignmentMap::default();
        assignment.set(loaded, PhysReg::RAX);
        assignment.set(index, PhysReg::RCX);
        assignment.set(table_base, PhysReg::RDX);
        assignment.set(target, PhysReg::RBX);
        let emitted = emit(&function, &assignment, 0).unwrap();
        let code = crate::jit_mem::JitCode::new(&emitted.code).unwrap();
        for index in 0..4_u8 {
            let mut state = [0xfc | index];
            assert_eq!(unsafe { code.call(&mut state) }, i64::from(index) + 1);
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_sparse_commit_tail_chunk() {
        const STABLE: usize = 0;
        const SPARSE: usize = 16;
        const DIRTY: usize = 32;
        const SUMMARY: usize = 40;
        let mut function = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::SparseCommit {
            src_offset: SPARSE as i32,
            dst_offset: STABLE as i32,
            byte_size: 13,
            dirty_words_offset: DIRTY as i32,
            dirty_word_count: 1,
            summary_words_offset: SUMMARY as i32,
            summary_word_count: 1,
            four_state: false,
        });
        block.push(MInst::Return);
        function.push_block(block);
        let emitted = emit(&function, &AssignmentMap::default(), 0).unwrap();
        let code = crate::jit_mem::JitCode::new(&emitted.code).unwrap();
        let mut state = vec![0x11_u8; 64];
        state[SPARSE..SPARSE + 13].copy_from_slice(&[0xa5; 13]);
        state[DIRTY..DIRTY + 8].copy_from_slice(&2_u64.to_le_bytes());
        state[SUMMARY..SUMMARY + 8].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        assert_eq!(&state[STABLE..STABLE + 8], &[0x11; 8]);
        assert_eq!(&state[STABLE + 8..STABLE + 13], &[0xa5; 5]);
        assert_eq!(&state[DIRTY..DIRTY + 8], &[0; 8]);
        assert_eq!(&state[SUMMARY..SUMMARY + 8], &[0; 8]);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_sparse_worklist_commit() {
        const BYTE_SIZE: usize = 13;
        const STABLE: usize = 0;
        const SPARSE: usize = 32;
        const DIRTY: usize = 64;
        const SUMMARY: usize = 72;
        const ACTIVE: usize = 80;
        let mut function = MFunction::new(VRegAllocator::new(), Vec::new());
        let table = function.intern_constant_table(
            SparseCommitDescriptor {
                src_offset: SPARSE as u64,
                dst_offset: STABLE as u64,
                byte_size: BYTE_SIZE as u64,
                dirty_words_offset: DIRTY as u64,
                dirty_word_count: 1,
                summary_words_offset: SUMMARY as u64,
                summary_word_count: 1,
                four_state: 1,
            }
            .words()
            .to_vec(),
        );
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::SparseMarkActive {
            active_index: 0,
            active_bits_offset: ACTIVE as i32,
            active_capacity: 1,
        });
        block.push(MInst::SparseCommitWorklist {
            descriptor_table: table,
            active_bits_offset: ACTIVE as i32,
            active_capacity: 1,
        });
        block.push(MInst::Return);
        function.push_block(block);
        let emitted = emit(&function, &AssignmentMap::default(), 0).unwrap();
        let code = crate::jit_mem::JitCode::new(&emitted.code).unwrap();
        let mut state = vec![0_u8; 128];
        for index in 0..BYTE_SIZE * 2 {
            state[SPARSE + index] = index as u8 ^ 0x6d;
        }
        state[DIRTY..DIRTY + 8].copy_from_slice(&3_u64.to_le_bytes());
        state[SUMMARY..SUMMARY + 8].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        assert_eq!(&state[ACTIVE..ACTIVE + 8], &[0; 8]);
        assert_eq!(&state[SUMMARY..SUMMARY + 8], &[0; 8]);
        assert_eq!(&state[DIRTY..DIRTY + 8], &[0; 8]);
        assert_eq!(
            &state[STABLE..STABLE + BYTE_SIZE * 2],
            &state[SPARSE..SPARSE + BYTE_SIZE * 2]
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_sparse_active_mark() {
        let mut function = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::SparseMarkActive {
            active_index: 3,
            active_bits_offset: 16,
            active_capacity: 4,
        });
        block.push(MInst::Return);
        function.push_block(block);
        let emitted = emit(&function, &AssignmentMap::default(), 0).unwrap();
        let code = crate::jit_mem::JitCode::new(&emitted.code).unwrap();
        let mut state = [0_u8; 24];
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        assert_eq!(u64::from_le_bytes(state[16..].try_into().unwrap()), 8);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_stack_parallel_copy_cycle_without_clobbering_payloads() {
        let mut vregs = VRegAllocator::new();
        let first = vregs.alloc();
        let second = vregs.alloc();
        let immediate = vregs.alloc();
        let dst_first = vregs.alloc();
        let dst_second = vregs.alloc();
        let dst_copy = vregs.alloc();
        let dst_immediate = vregs.alloc();
        let out_first = vregs.alloc();
        let out_second = vregs.alloc();
        let out_copy = vregs.alloc();
        let out_immediate = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 11]);

        let mut predecessor = MBlock::new(BlockId(0));
        predecessor.push(MInst::LoadImm {
            dst: first,
            value: 33,
        });
        predecessor.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 0,
            src: first,
            size: OpSize::S64,
        });
        predecessor.push(MInst::LoadImm {
            dst: second,
            value: 44,
        });
        predecessor.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 8,
            src: second,
            size: OpSize::S64,
        });
        predecessor.push(MInst::LoadImm {
            dst: immediate,
            value: 55,
        });
        predecessor.push(MInst::Jump { target: BlockId(1) });

        let mut successor = MBlock::new(BlockId(1));
        successor.phis = vec![
            PhiNode {
                dst: dst_first,
                sources: vec![(BlockId(0), second)],
            },
            PhiNode {
                dst: dst_second,
                sources: vec![(BlockId(0), first)],
            },
            PhiNode {
                dst: dst_copy,
                sources: vec![(BlockId(0), first)],
            },
            PhiNode {
                dst: dst_immediate,
                sources: vec![(BlockId(0), immediate)],
            },
        ];
        for (stack_offset, state_offset, output) in [
            (0, 0, out_first),
            (8, 8, out_second),
            (16, 16, out_copy),
            (24, 24, out_immediate),
        ] {
            successor.push(MInst::Load {
                dst: output,
                base: BaseReg::StackFrame,
                offset: stack_offset,
                size: OpSize::S64,
            });
            successor.push(MInst::Store {
                base: BaseReg::SimState,
                offset: state_offset,
                src: output,
                size: OpSize::S64,
            });
        }
        successor.push(MInst::Return);
        function.push_block(predecessor);
        function.push_block(successor);

        let mut assignment = AssignmentMap::default();
        for (value, register) in [
            (first, PhysReg::RAX),
            (second, PhysReg::RDX),
            (immediate, PhysReg::RSI),
            (out_first, PhysReg::RAX),
            (out_second, PhysReg::RAX),
            (out_copy, PhysReg::RAX),
            (out_immediate, PhysReg::RAX),
        ] {
            assignment.set(value, register);
        }
        assignment.set_edge_location(BlockId(0), first, EdgeLocation::Stack(0));
        assignment.set_edge_location(BlockId(0), second, EdgeLocation::Stack(8));
        assignment.set_edge_location(BlockId(0), immediate, EdgeLocation::Immediate(77));
        assignment.set_edge_spill_slot(dst_first, 0);
        assignment.set_edge_spill_slot(dst_second, 8);
        assignment.set_edge_spill_slot(dst_copy, 16);
        assignment.set_edge_spill_slot(dst_immediate, 24);

        let emitted = emit(&function, &assignment, 32).unwrap();
        let code = crate::jit_mem::JitCode::new(&emitted.code).unwrap();
        let mut state = vec![0_u8; 32];
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        let actual = state
            .chunks_exact(8)
            .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(actual, [44, 33, 33, 77]);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_packed_compare_fallbacks() {
        let mut vregs = VRegAllocator::new();
        let rhs = vregs.alloc();
        let lanes = vregs.alloc();
        let base = vregs.alloc();
        let affine_rhs = vregs.alloc();
        let affine = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: rhs, value: 5 });
        block.push(MInst::PackedLaneCompare {
            dst: lanes,
            rhs: PackedLaneCompareRhs::Scalar(rhs),
            kind: CmpKind::Eq,
            offset: 0,
            lane_count: 16,
            element_stride: 1,
            bit_offset: 0,
            field_width: 8,
            alias_range: None,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 24,
            src: lanes,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: base,
            value: 250,
        });
        block.push(MInst::LoadImm {
            dst: affine_rhs,
            value: 2,
        });
        block.push(MInst::PackedByteAffineCompare {
            dst: affine,
            base,
            rhs: affine_rhs,
            kind: CmpKind::Eq,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 32,
            src: affine,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        function.push_block(block);
        let mut assignment = AssignmentMap::default();
        for (value, register) in [
            (rhs, PhysReg::RAX),
            (lanes, PhysReg::RDX),
            (base, PhysReg::RSI),
            (affine_rhs, PhysReg::RDI),
            (affine, PhysReg::R8),
        ] {
            assignment.set(value, register);
        }
        let emitted = emit(&function, &assignment, 0).unwrap();
        let code = crate::jit_mem::JitCode::new(&emitted.code).unwrap();
        let mut state = vec![0_u8; 40];
        state[..16].copy_from_slice(&[5, 1, 5, 2, 3, 5, 4, 5, 6, 7, 8, 9, 5, 5, 0, 5]);
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[24..32].try_into().unwrap()),
            0b1011_0000_1010_0101
        );
        assert_eq!(
            u64::from_le_bytes(state[32..40].try_into().unwrap()),
            1 << 8
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_parallel_bit_extract_and_deposit() {
        let mut vregs = VRegAllocator::new();
        let source = vregs.alloc();
        let mask = vregs.alloc();
        let extracted = vregs.alloc();
        let deposited_source = vregs.alloc();
        let deposited = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: source,
            value: 0b110101,
        });
        block.push(MInst::LoadImm {
            dst: mask,
            value: 0b101010,
        });
        block.push(MInst::Pext {
            dst: extracted,
            src: source,
            mask,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: extracted,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: deposited_source,
            value: 0b101,
        });
        block.push(MInst::Pdep {
            dst: deposited,
            src: deposited_source,
            mask,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: deposited,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        function.push_block(block);
        let mut assignment = AssignmentMap::default();
        for (value, register) in [
            (source, PhysReg::RAX),
            (mask, PhysReg::RDX),
            (extracted, PhysReg::RSI),
            (deposited_source, PhysReg::RDI),
            (deposited, PhysReg::R8),
        ] {
            assignment.set(value, register);
        }
        let emitted = emit(&function, &assignment, 0).unwrap();
        let code = crate::jit_mem::JitCode::new(&emitted.code).unwrap();
        let mut state = [0_u8; 16];
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        assert_eq!(u64::from_le_bytes(state[..8].try_into().unwrap()), 0b100);
        assert_eq!(u64::from_le_bytes(state[8..].try_into().unwrap()), 0b100010);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executes_native_tick_loop_inside_one_jit_call() {
        let mut vregs = VRegAllocator::new();
        let current = vregs.alloc();
        let one = vregs.alloc();
        let next = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: current,
            base: BaseReg::SimState,
            offset: 32,
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
            offset: 32,
            src: next,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        function.push_block(block);
        let mut assignment = AssignmentMap::default();
        assignment.set(current, PhysReg::RAX);
        assignment.set(one, PhysReg::RDX);
        assignment.set(next, PhysReg::RSI);
        let allocated = crate::legacy_allocation::adapt(&function, &assignment, 0).unwrap();
        let emitted = emit_function(
            &allocated.function,
            &allocated.assignment,
            0,
            64,
            &allocated.edge_copies,
            true,
            false,
        )
        .unwrap();
        let code = crate::jit_mem::JitCode::new(&emitted.code).unwrap();
        let mut state = [0_u8; 40];
        state[STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET
            ..STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET + 8]
            .copy_from_slice(&5_u64.to_le_bytes());
        assert_eq!(unsafe { code.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(
                state[STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET
                    ..STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET + 8]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        assert_eq!(u64::from_le_bytes(state[32..].try_into().unwrap()), 5);
    }
}
