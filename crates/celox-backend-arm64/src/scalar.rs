//! AArch64 SIR lowering, register allocation, and machine-code emission.

// dynasm's dynamic-register syntax expands through an Into conversion even
// when the register number is already a u8.
#![allow(clippy::useless_conversion)]

use std::{collections::BTreeSet, fmt};

use celox_state_layout::{
    STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET, STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET,
    STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
};
use dynasmrt::aarch64::Aarch64Relocation;
use dynasmrt::{DynamicLabel, DynasmApi, DynasmError, DynasmLabelApi, VecAssembler, dynasm};

use crate::allocation::{Assignment, CopyDestination, CopyOperation, CopySource, EdgeCopyPlan};
use crate::mir::{
    BaseReg, BlockId, BranchPredicate, CmpKind, MFunction, MInst, OpSize, PackedLaneCompareRhs,
    SPARSE_COMMIT_DESCRIPTOR_WORDS, VReg,
};
use crate::{Arm64Reg, HashMap};

const STATE_REG: u8 = 0;
const SCRATCH0: u8 = 16;
const SCRATCH1: u8 = 17;
// x28 is reserved as the base of the target-owned spill frame.  Keeping the
// frame base live avoids materializing `state + spill_base` for every reload
// and spill store when the simulator state is larger than AArch64's immediate
// addressing range.
const SPILL_REG: u8 = 28;
const STATE_PAGE_REG: u8 = 29;
const STATE_PAGE_BYTES: i64 = 4096;
const MAX_SECONDARY_STATE_PAGES: usize = 4;
const MIN_STATE_PAGE_BENEFIT: usize = 8;
const TEMPORARY_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StatePageBases {
    primary: Option<i64>,
    secondary: [Option<(u8, i64)>; MAX_SECONDARY_STATE_PAGES],
}

#[derive(Clone, Copy, Debug)]
enum StatePageAccess {
    Direct {
        offset: i64,
        size: OpSize,
        store: bool,
    },
    Indexed {
        offset: i64,
        size: OpSize,
        store: bool,
    },
    Vector {
        offset: i64,
    },
}

impl StatePageAccess {
    fn offset(self) -> i64 {
        match self {
            Self::Direct { offset, .. }
            | Self::Indexed { offset, .. }
            | Self::Vector { offset } => offset,
        }
    }
}

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
    Lowering(String),
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
            Self::Lowering(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for EmitError {}

impl From<DynasmError> for EmitError {
    fn from(error: DynasmError) -> Self {
        Self::Assembly(error)
    }
}

#[derive(Debug)]
pub enum PrepareError {
    Sir(celox_sir::verify::SirVerifyError),
    StateSizeOverflow,
}

impl fmt::Display for PrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sir(error) => write!(formatter, "at AArch64 backend boundary: {error}"),
            Self::StateSizeOverflow => {
                formatter.write_str("AArch64 simulation-state size overflow")
            }
        }
    }
}

impl std::error::Error for PrepareError {}

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
    sir_eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    layout: &crate::MemoryLayout,
    four_state: bool,
    label: &str,
    native_tick_loop: bool,
    mut trace: Option<&mut NativeFunctionTrace>,
) -> Result<EmitResult, ChainedEmitError> {
    sir_eu.verify_result().map_err(PrepareError::Sir)?;
    let tick_loop = label == "eval_comb_apply_ff" && native_tick_loop;
    let check_runtime_events = tick_loop
        && sir_eu.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    crate::SIRInstruction::RuntimeEvent { .. }
                        | crate::SIRInstruction::CombCaptureEvent { .. }
                )
            })
        });
    if let Some(trace) = trace.as_deref_mut() {
        trace.optimized_sir = sir_eu.to_string();
    }
    let state_size = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .ok_or(PrepareError::StateSizeOverflow)?;
    let mut function = crate::isel::lower_execution_unit(sir_eu, layout, four_state);
    crate::mir_opt::optimize(&mut function);
    crate::mir_legalize::legalize_variable_shift_counts(&mut function);
    if let Some(trace) = trace.as_deref_mut() {
        trace.mir_before_regalloc = format!("{function:#?}");
    }
    let allocation = crate::regalloc::allocate_with_spills(function)
        .map_err(|error| EmitError::Lowering(error.to_string()))?;
    if let Some(trace) = trace.as_deref_mut() {
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

/// Emit the canonical no-op simulation kernel directly in AArch64 MIR.
pub fn emit_empty(state_size: usize) -> Result<EmitResult, EmitError> {
    let mut block = crate::mir::MBlock::new(BlockId(0));
    block.push(MInst::Return);
    let function = MFunction::new(vec![block], Vec::new());
    let allocation = crate::regalloc::allocate_with_spills(function)
        .map_err(|error| EmitError::Lowering(error.to_string()))?;
    emit_function(
        &allocation.allocated.function,
        &allocation.allocated.assignment,
        allocation.spill_frame_size,
        state_size,
        &allocation.allocated.edge_copies,
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
    // Use x29 for the most frequently accessed state page so large state
    // offsets can use ordinary AArch64 memory immediates without reducing the
    // allocator's general-purpose register file.  If a preserved register is
    // unused by this function, cache a second hot page there as well.  The
    // tick loop normally uses x29 for its counter; when a page base is
    // profitable, keep that counter in an otherwise-unused SIMD scalar
    // register instead.
    let secondary_page_registers = (19..=27).rev().filter(|register| {
        !assignment
            .iter()
            .any(|(_, value)| value.number() == *register)
    });
    let state_pages = select_state_base_pages(function, secondary_page_registers);
    let tick_counter_in_fp = tick_loop && state_pages.primary.is_some();
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
        .filter(|register| (19..=27).contains(register))
        .collect::<Vec<_>>();
    if tick_loop || state_pages.primary.is_some() {
        // x29 is reserved for either the native-loop counter or the cached
        // state-page base, so preserve the host value across the JIT call.
        callee_saved.push(STATE_PAGE_REG);
    }
    for &(register, _) in state_pages.secondary.iter().flatten() {
        callee_saved.push(register);
    }
    // x28 is the fixed base for the spill/cycle-break arena, including the
    // temporary slot used by parallel-copy resolution even when no value was
    // spilled.
    callee_saved.push(SPILL_REG);
    callee_saved.sort_unstable();
    callee_saved.dedup();

    if let Some(&first) = callee_saved.first() {
        dynasm!(ops ; .arch aarch64 ; stp x30, X(first), [sp, #-16]!);
        let mut index = 1;
        while index + 1 < callee_saved.len() {
            dynasm!(ops
                ; .arch aarch64
                ; stp X(callee_saved[index]), X(callee_saved[index + 1]), [sp, #-16]!
            );
            index += 2;
        }
        if index < callee_saved.len() {
            dynasm!(ops ; .arch aarch64 ; str X(callee_saved[index]), [sp, #-16]!);
        }
    } else {
        dynasm!(ops ; .arch aarch64 ; str x30, [sp, #-16]!);
    }
    if tick_loop {
        // d29 retains the simulator-state pointer across success/error return
        // values. Keep the remaining tick count in x29 unless x29 is needed
        // for the cached state-page base.
        dynasm!(ops ; .arch aarch64 ; fmov d29, x0);
        emit_address(
            &mut ops,
            STATE_REG,
            STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET as i64,
        );
        emit_load(&mut ops, SCRATCH0, SCRATCH0, OpSize::S64);
        let count_ready = ops.new_dynamic_label();
        if tick_counter_in_fp {
            dynasm!(ops
                ; .arch aarch64
                ; cbnz x16, =>count_ready
                ; mov x16, #1
                ; =>count_ready
                ; fmov d31, x16
            );
        } else {
            dynasm!(ops
                ; .arch aarch64
                ; cbnz x16, =>count_ready
                ; mov x16, #1
                ; =>count_ready
                ; mov x29, x16
            );
        }
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
    emit_address_to(&mut ops, SPILL_REG, STATE_REG, spill_base as i64);
    if let Some(page) = state_pages.primary {
        emit_address_to(&mut ops, STATE_PAGE_REG, STATE_REG, page);
    }
    for &(register, page) in state_pages.secondary.iter().flatten() {
        emit_address_to(&mut ops, register, STATE_REG, page);
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
                state_pages,
                tick_counter_in_fp,
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
        if tick_counter_in_fp {
            dynasm!(ops ; .arch aarch64 ; fmov x16, d31 ; fmov x17, d29);
        } else {
            dynasm!(ops ; .arch aarch64 ; mov x16, x29 ; fmov x17, d29);
        }
        emit_load_imm(
            &mut ops,
            30,
            STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET as u64,
        );
        dynasm!(ops ; .arch aarch64 ; add x17, x17, x30 ; str x16, [x17]);
    }
    if callee_saved.is_empty() {
        dynasm!(ops ; .arch aarch64 ; ldr x30, [sp], #16);
    } else {
        let mut index = callee_saved.len();
        if index > 1 && (index - 1) % 2 == 1 {
            index -= 1;
            dynasm!(ops ; .arch aarch64 ; ldr X(callee_saved[index]), [sp], #16);
        }
        while index > 1 {
            index -= 2;
            dynasm!(ops
                ; .arch aarch64
                ; ldp X(callee_saved[index]), X(callee_saved[index + 1]), [sp], #16
            );
        }
        dynasm!(ops ; .arch aarch64 ; ldp x30, X(callee_saved[0]), [sp], #16);
    }
    dynasm!(ops ; .arch aarch64 ; ret);
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
    state_pages: StatePageBases,
    tick_counter_in_fp: bool,
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
            let offset = base_offset(*base, *offset);
            let destination = resolve(assignment, *dst)?;
            let (base_register, offset) =
                select_memory_base(*base, offset, destination, *size, false, state_pages);
            emit_load_at(ops, destination, base_register, offset, *size);
        }
        MInst::Store {
            base,
            offset,
            src,
            size,
        } => {
            let offset = base_offset(*base, *offset);
            let source = resolve(assignment, *src)?;
            let (base_register, offset) =
                select_memory_base(*base, offset, source, *size, true, state_pages);
            emit_store_at(ops, source, base_register, offset, *size);
        }
        MInst::LoadPtr {
            dst,
            ptr,
            offset,
            size,
        } => {
            emit_load_at(
                ops,
                resolve(assignment, *dst)?,
                resolve(assignment, *ptr)?,
                i64::from(*offset),
                *size,
            );
        }
        MInst::StorePtr {
            ptr,
            offset,
            src,
            size,
        } => {
            emit_store_at(
                ops,
                resolve(assignment, *src)?,
                resolve(assignment, *ptr)?,
                i64::from(*offset),
                *size,
            );
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
            let index = resolve(assignment, *index)?;
            let destination = resolve(assignment, *dst)?;
            let (base, offset) =
                select_indexed_memory_base(*base, i64::from(*offset), *size, false, state_pages);
            emit_load_indexed_at(ops, destination, base, index, offset, *scale, *size);
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
            let index = resolve(assignment, *index)?;
            if matches!(instruction, MInst::OrStoreIndexed { .. }) {
                let src = resolve(assignment, *src)?;
                let (base, offset) = select_indexed_memory_base(
                    *base,
                    i64::from(*offset),
                    *size,
                    false,
                    state_pages,
                );
                if offset == 0 {
                    emit_load_indexed_at(ops, SCRATCH1, base, index, 0, 1, *size);
                    dynasm!(ops ; .arch aarch64 ; orr x17, x17, X(src));
                    emit_store_indexed_at(ops, SCRATCH1, base, index, 0, 1, *size);
                } else {
                    dynasm!(ops ; .arch aarch64 ; add x16, X(base), X(index));
                    emit_add_offset(ops, offset);
                    emit_load(ops, SCRATCH1, SCRATCH0, *size);
                    dynasm!(ops ; .arch aarch64 ; orr x17, x17, X(src));
                    emit_store(ops, SCRATCH1, SCRATCH0, *size);
                }
            } else {
                let src = resolve(assignment, *src)?;
                let (base, offset) =
                    select_indexed_memory_base(*base, i64::from(*offset), *size, true, state_pages);
                emit_store_indexed_at(ops, src, base, index, offset, 1, *size);
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
            emit_load_indexed_at(
                ops,
                resolve(assignment, *dst)?,
                ptr,
                index,
                i64::from(*offset),
                1,
                *size,
            );
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
            let src = resolve(assignment, *src)?;
            if matches!(instruction, MInst::ReleaseStorePtrIndexed { .. }) {
                dynasm!(ops ; .arch aarch64 ; add x16, X(ptr), X(index));
                emit_add_offset(ops, i64::from(*offset));
                emit_release_store(ops, src, SCRATCH0, *size);
            } else {
                emit_store_indexed_at(ops, src, ptr, index, i64::from(*offset), 1, *size);
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
            let address_offset = base_offset(*base, *offset);
            let (base_register, address_offset) =
                select_memory_base(*base, address_offset, SCRATCH1, *size, false, state_pages);
            if memory_access_encoding(SCRATCH1, base_register, address_offset, *size, false)
                .is_some()
            {
                emit_load_at(ops, SCRATCH1, base_register, address_offset, *size);
                emit_load_imm(ops, SCRATCH0, *imm);
                if matches!(instruction, MInst::AndStoreImm { .. }) {
                    dynasm!(ops ; .arch aarch64 ; and x30, x17, x16);
                } else {
                    dynasm!(ops ; .arch aarch64 ; orr x30, x17, x16);
                }
                emit_store_at(ops, 30, base_register, address_offset, *size);
            } else {
                emit_base_address(ops, *base, *offset)?;
                emit_load(ops, SCRATCH1, SCRATCH0, *size);
                emit_load_imm(ops, SCRATCH0, *imm);
                if matches!(instruction, MInst::AndStoreImm { .. }) {
                    dynasm!(ops ; .arch aarch64 ; and x30, x17, x16);
                } else {
                    dynasm!(ops ; .arch aarch64 ; orr x30, x17, x16);
                }
                emit_base_address(ops, *base, *offset)?;
                emit_store(ops, 30, SCRATCH0, *size);
            }
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
            let is_and = matches!(instruction, MInst::AndImm { .. });
            if !emit_logical_immediate(ops, dst, src, *imm, 64, is_and) {
                emit_load_imm(ops, SCRATCH0, *imm);
                if is_and {
                    dynasm!(ops ; .arch aarch64 ; and X(dst), X(src), x16);
                } else {
                    dynasm!(ops ; .arch aarch64 ; orr X(dst), X(src), x16);
                }
            }
        }
        MInst::AndImm32 { dst, src, imm } => {
            let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
            if !emit_logical_immediate(ops, dst, src, u64::from(*imm), 32, true) {
                emit_load_imm(ops, SCRATCH0, u64::from(*imm));
                dynasm!(ops ; .arch aarch64 ; and W(dst), W(src), w16);
            }
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
            let offset = if matches!(instruction, MInst::SubImm { .. }) {
                -i64::from(*imm)
            } else {
                i64::from(*imm)
            };
            if !emit_add_sub_immediate(ops, dst, src, offset) {
                emit_load_imm(ops, SCRATCH0, offset.unsigned_abs());
                if offset < 0 {
                    dynasm!(ops ; .arch aarch64 ; sub X(dst), X(src), x16);
                } else {
                    dynasm!(ops ; .arch aarch64 ; add X(dst), X(src), x16);
                }
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
            if !emit_cmp_immediate(ops, lhs, *imm) {
                emit_load_imm(ops, SCRATCH0, *imm as i64 as u64);
                dynasm!(ops ; .arch aarch64 ; cmp X(lhs), x16);
            }
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
            if !emit_cmp_immediate(ops, lhs, *imm) {
                emit_load_imm(ops, SCRATCH0, *imm as i64 as u64);
                dynasm!(ops ; .arch aarch64 ; cmp X(lhs), x16);
            }
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
            emit_branch_predicate(ops, *predicate, assignment, state_pages)?;
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
                if tick_counter_in_fp {
                    dynasm!(ops
                        ; .arch aarch64
                        ; fmov x16, d31
                        ; subs x16, x16, #1
                        ; fmov d31, x16
                        ; b.eq =>success
                    );
                } else {
                    dynasm!(ops
                        ; .arch aarch64
                        ; subs x29, x29, #1
                        ; b.eq =>success
                    );
                }
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
                if tick_counter_in_fp {
                    dynasm!(ops
                        ; .arch aarch64
                        ; fmov x16, d31
                        ; sub x16, x16, #1
                        ; fmov d31, x16
                    );
                } else {
                    dynasm!(ops
                        ; .arch aarch64
                        ; sub x29, x29, #1
                    );
                }
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
            state_pages,
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
        } => emit_mem_copy(ops, *src_offset, *dst_offset, *byte_len, state_pages),
        MInst::MemFill {
            dst_offset,
            byte_len,
            value,
        } => emit_mem_fill(ops, *dst_offset, *byte_len, *value, state_pages),
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
            let offset = i64::from(offset);
            let (base, offset) = select_memory_base(
                BaseReg::SimState,
                offset,
                SCRATCH1,
                OpSize::S64,
                false,
                state_pages,
            );
            emit_load_at(ops, SCRATCH1, base, offset, OpSize::S64);
            emit_load_imm(ops, SCRATCH0, 1_u64 << (*active_index % 64));
            dynasm!(ops ; .arch aarch64 ; orr x30, x17, x16);
            let (base, offset) = select_memory_base(
                BaseReg::SimState,
                i64::from(*active_bits_offset) + i64::from(word_offset),
                30,
                OpSize::S64,
                true,
                state_pages,
            );
            emit_store_at(ops, 30, base, offset, OpSize::S64);
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
    state_pages: StatePageBases,
) {
    if byte_len == 0 || src_offset == dst_offset {
        return;
    }
    if mem_copy_uses_forward_vectors(src_offset, dst_offset, byte_len) {
        emit_mem_copy_forward_vectors(ops, src_offset, dst_offset, byte_len, state_pages);
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

fn emit_mem_copy_forward_vectors(
    ops: &mut VecAssembler<Aarch64Relocation>,
    src_offset: i32,
    dst_offset: i32,
    byte_len: usize,
    state_pages: StatePageBases,
) {
    let (src_base, src_relative) =
        select_vector_memory_base(BaseReg::SimState, i64::from(src_offset), state_pages);
    emit_address_to(ops, SCRATCH0, src_base, src_relative);
    let (dst_base, dst_relative) =
        select_vector_memory_base(BaseReg::SimState, i64::from(dst_offset), state_pages);
    emit_address_to(ops, SCRATCH1, dst_base, dst_relative);

    let vector_chunks = byte_len / 16;
    for _ in 0..vector_chunks {
        dynasm!(ops
            ; .arch aarch64
            ; ldr q0, [x16], #16
            ; str q0, [x17], #16
        );
    }
    let remainder = byte_len % 16;
    if remainder >= 8 {
        dynasm!(ops ; .arch aarch64 ; ldr x30, [x16], #8 ; str x30, [x17], #8);
    }
    if remainder % 8 >= 4 {
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
    if destination != source && destination != mask {
        emit_parallel_bits_loop(ops, destination, source, mask, deposit);
        return;
    }

    emit_parallel_bits_saved_loop(ops, destination, source, mask, deposit);
}

fn emit_parallel_bits_loop(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    source: u8,
    mask: u8,
    deposit: bool,
) {
    let loop_label = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; .arch aarch64
        ; mov X(destination), xzr
        ; mov x16, X(mask)
        ; mov x17, xzr
        ; =>loop_label
        ; cbz x16, =>done
        ; rbit x30, x16
        ; clz x30, x30
        ; fmov d4, x30
        ; sub x30, x16, #1
        ; and x16, x16, x30
    );
    if deposit {
        // x17 counts source bits while d4 retains the destination bit
        // selected by the lowest set mask bit. d5 protects the working mask
        // while x16 is reused as the variable shift amount.
        dynasm!(ops
            ; .arch aarch64
            ; fmov d5, x16
            ; lsrv x30, X(source), x17
            ; and x30, x30, #1
            ; fmov x16, d4
            ; lslv x30, x30, x16
            ; orr X(destination), X(destination), x30
            ; fmov x16, d5
            ; add x17, x17, #1
            ; b =>loop_label
            ; =>done
        );
    } else {
        dynasm!(ops
            ; .arch aarch64
            ; fmov x30, d4
            ; lsrv x30, X(source), x30
            ; and x30, x30, #1
            ; lslv x30, x30, x17
            ; orr X(destination), X(destination), x30
            ; add x17, x17, #1
            ; b =>loop_label
            ; =>done
        );
    }
}

fn emit_parallel_bits_saved_loop(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    source: u8,
    mask: u8,
    deposit: bool,
) {
    // Save both inputs before defining the result: allocation may coalesce a
    // dying input with the destination. The result can then reuse the
    // destination register, while d4-d7 hold the transient scalar values.
    let loop_label = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; .arch aarch64
        ; fmov d6, X(source)
        ; fmov d7, X(mask)
        ; mov X(destination), xzr
        ; mov x17, xzr
        ; =>loop_label
        ; fmov x16, d7
        ; cbz x16, =>done
        ; rbit x30, x16
        ; clz x30, x30
        ; fmov d4, x30
        ; sub x30, x16, #1
        ; and x16, x16, x30
        ; fmov d7, x16
    );
    if deposit {
        dynasm!(ops
            ; .arch aarch64
            ; fmov x16, d6
            ; lsrv x30, x16, x17
            ; and x30, x30, #1
            ; fmov x16, d4
            ; lslv x30, x30, x16
            ; orr X(destination), X(destination), x30
            ; add x17, x17, #1
            ; b =>loop_label
            ; =>done
        );
    } else {
        dynasm!(ops
            ; .arch aarch64
            ; fmov x16, d6
            ; fmov x30, d4
            ; lsrv x30, x16, x30
            ; and x30, x30, #1
            ; lslv x30, x30, x17
            ; orr X(destination), X(destination), x30
            ; add x17, x17, #1
            ; b =>loop_label
            ; =>done
        );
    }
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
    state_pages: StatePageBases,
) -> Result<(), EmitError> {
    let size = match element_stride {
        1 => OpSize::S8,
        2 => OpSize::S16,
        4 => OpSize::S32,
        _ => return Err(EmitError::Range("packed lane stride must be 1, 2, or 4")),
    };
    if usize::from(lane_count) * usize::from(element_stride) % 16 == 0 {
        emit_packed_lane_compare_vector(
            ops,
            destination,
            rhs,
            kind,
            offset,
            lane_count,
            element_stride,
            bit_offset,
            field_width,
            assignment,
            state_pages,
        )?;
        return Ok(());
    }
    let scalar_rhs = match rhs {
        PackedLaneCompareRhs::Scalar(value) => Some(resolve(assignment, value)?),
        PackedLaneCompareRhs::Memory { .. } => None,
    };
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
        let (base, offset) = select_memory_base(
            BaseReg::SimState,
            i64::from(lane_delta),
            SCRATCH0,
            size,
            false,
            state_pages,
        );
        emit_load_at(ops, SCRATCH0, base, offset, size);
        match rhs {
            PackedLaneCompareRhs::Scalar(_) => {
                dynasm!(ops ; .arch aarch64 ; mov x17, X(scalar_rhs.unwrap()));
            }
            PackedLaneCompareRhs::Memory { offset, .. } => {
                let rhs_offset = i32::from(lane)
                    .checked_mul(i32::from(element_stride))
                    .and_then(|delta| offset.checked_add(delta))
                    .ok_or(EmitError::Range("packed lane RHS offset overflow"))?;
                let (base, offset) = select_memory_base(
                    BaseReg::SimState,
                    i64::from(rhs_offset),
                    SCRATCH1,
                    size,
                    false,
                    state_pages,
                );
                let direct = memory_access_encoding(SCRATCH1, base, offset, size, false).is_some();
                if !direct {
                    dynasm!(ops ; .arch aarch64 ; fmov d7, x16);
                }
                emit_load_at(ops, SCRATCH1, base, offset, size);
                if !direct {
                    dynasm!(ops ; .arch aarch64 ; fmov x16, d7);
                }
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
            if logical_immediate_encoding(field_mask, 64, SCRATCH0, SCRATCH0, true).is_some() {
                emit_logical_immediate(ops, SCRATCH0, SCRATCH0, field_mask, 64, true);
                emit_logical_immediate(ops, SCRATCH1, SCRATCH1, field_mask, 64, true);
            } else {
                dynasm!(ops ; .arch aarch64 ; fmov d5, x30);
                emit_load_imm(ops, 30, field_mask);
                dynasm!(ops
                    ; .arch aarch64
                    ; and x16, x16, x30
                    ; and x17, x17, x30
                    ; fmov x30, d5
                );
            }
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

#[allow(clippy::too_many_arguments)]
fn emit_packed_lane_compare_vector(
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
    state_pages: StatePageBases,
) -> Result<(), EmitError> {
    let lanes_per_vector = 16 / usize::from(element_stride);
    let storage_width = element_stride * 8;
    let scalar_rhs = match rhs {
        PackedLaneCompareRhs::Scalar(value) => Some(resolve(assignment, value)?),
        PackedLaneCompareRhs::Memory { .. } => None,
    };
    let needs_mask = field_width != storage_width;
    let field_mask = if field_width == 64 {
        u64::MAX
    } else {
        (1_u64 << field_width) - 1
    };

    if needs_mask {
        emit_load_imm(ops, SCRATCH1, field_mask);
        match element_stride {
            1 => dynasm!(ops ; .arch aarch64 ; dup v4.b16, W(SCRATCH1)),
            2 => dynasm!(ops ; .arch aarch64 ; dup v4.h8, W(SCRATCH1)),
            4 => dynasm!(ops ; .arch aarch64 ; dup v4.s4, W(SCRATCH1)),
            _ => unreachable!(),
        }
    }
    dynasm!(ops ; .arch aarch64 ; mov x30, xzr);

    for lane_base in (0..usize::from(lane_count)).step_by(lanes_per_vector) {
        let chunk_delta = i32::try_from(lane_base * usize::from(element_stride))
            .map_err(|_| EmitError::Range("packed lane offset overflow"))?;
        let lhs_offset = offset
            .checked_add(chunk_delta)
            .ok_or(EmitError::Range("packed lane offset overflow"))?;
        let (lhs_base, lhs_offset) =
            select_vector_memory_base(BaseReg::SimState, i64::from(lhs_offset), state_pages);
        emit_vector_load(ops, 0, lhs_base, lhs_offset);

        if let PackedLaneCompareRhs::Memory {
            offset: rhs_offset, ..
        } = rhs
        {
            let rhs_offset = rhs_offset
                .checked_add(chunk_delta)
                .ok_or(EmitError::Range("packed lane RHS offset overflow"))?;
            let (rhs_base, rhs_offset) =
                select_vector_memory_base(BaseReg::SimState, i64::from(rhs_offset), state_pages);
            emit_vector_load(ops, 1, rhs_base, rhs_offset);
        } else if let Some(rhs) = scalar_rhs {
            match element_stride {
                1 => dynasm!(ops ; .arch aarch64 ; dup v1.b16, W(rhs)),
                2 => dynasm!(ops ; .arch aarch64 ; dup v1.h8, W(rhs)),
                4 => dynasm!(ops ; .arch aarch64 ; dup v1.s4, W(rhs)),
                _ => unreachable!(),
            }
        }

        if bit_offset != 0 {
            let shift = u32::from(bit_offset);
            match element_stride {
                1 => dynasm!(ops ; .arch aarch64 ; ushr v0.b16, v0.b16, #shift),
                2 => dynasm!(ops ; .arch aarch64 ; ushr v0.h8, v0.h8, #shift),
                4 => dynasm!(ops ; .arch aarch64 ; ushr v0.s4, v0.s4, #shift),
                _ => unreachable!(),
            }
            match element_stride {
                1 => dynasm!(ops ; .arch aarch64 ; ushr v1.b16, v1.b16, #shift),
                2 => dynasm!(ops ; .arch aarch64 ; ushr v1.h8, v1.h8, #shift),
                4 => dynasm!(ops ; .arch aarch64 ; ushr v1.s4, v1.s4, #shift),
                _ => unreachable!(),
            }
        }
        if needs_mask {
            match element_stride {
                1 => dynasm!(ops
                    ; .arch aarch64
                    ; and v0.b16, v0.b16, v4.b16
                    ; and v1.b16, v1.b16, v4.b16
                ),
                2 => dynasm!(ops
                    ; .arch aarch64
                    ; and v0.b16, v0.b16, v4.b16
                    ; and v1.b16, v1.b16, v4.b16
                ),
                4 => dynasm!(ops
                    ; .arch aarch64
                    ; and v0.b16, v0.b16, v4.b16
                    ; and v1.b16, v1.b16, v4.b16
                ),
                _ => unreachable!(),
            }
        }

        emit_packed_vector_compare(ops, element_stride, kind);
        emit_packed_vector_mask(ops, element_stride);
        if lane_base != 0 {
            let shift = u32::try_from(lane_base)
                .map_err(|_| EmitError::Range("packed lane result shift overflow"))?;
            dynasm!(ops ; .arch aarch64 ; lsl x16, x16, #shift);
        }
        dynasm!(ops ; .arch aarch64 ; orr x30, x30, x16);
    }
    dynasm!(ops ; .arch aarch64 ; mov X(destination), x30);
    Ok(())
}

fn emit_vector_load(ops: &mut VecAssembler<Aarch64Relocation>, vector: u8, base: u8, offset: i64) {
    emit_address_to(ops, SCRATCH0, base, offset);
    match vector {
        0 => dynasm!(ops ; .arch aarch64 ; ldr q0, [x16]),
        1 => dynasm!(ops ; .arch aarch64 ; ldr q1, [x16]),
        _ => unreachable!(),
    }
}

fn emit_packed_vector_compare(
    ops: &mut VecAssembler<Aarch64Relocation>,
    element_stride: u8,
    kind: CmpKind,
) {
    macro_rules! compare {
        ($instruction:ident, $shape:ident) => {
            dynasm!(ops ; .arch aarch64 ; $instruction v0.$shape, v0.$shape, v1.$shape)
        };
    }
    macro_rules! compare_swapped {
        ($instruction:ident, $shape:ident) => {
            dynasm!(ops ; .arch aarch64 ; $instruction v0.$shape, v1.$shape, v0.$shape)
        };
    }
    macro_rules! invert {
        ($shape:ident) => {
            dynasm!(ops ; .arch aarch64 ; mvn v0.b16, v0.b16)
        };
    }

    match (element_stride, kind) {
        (1, CmpKind::Eq) => compare!(cmeq, b16),
        (2, CmpKind::Eq) => compare!(cmeq, h8),
        (4, CmpKind::Eq) => compare!(cmeq, s4),
        (1, CmpKind::Ne) => {
            compare!(cmeq, b16);
            invert!(b16);
        }
        (2, CmpKind::Ne) => {
            compare!(cmeq, h8);
            invert!(h8);
        }
        (4, CmpKind::Ne) => {
            compare!(cmeq, s4);
            invert!(s4);
        }
        (1, CmpKind::LtU | CmpKind::GeU) => {
            compare_swapped!(cmhi, b16);
            if matches!(kind, CmpKind::GeU) {
                invert!(b16);
            }
        }
        (2, CmpKind::LtU | CmpKind::GeU) => {
            compare_swapped!(cmhi, h8);
            if matches!(kind, CmpKind::GeU) {
                invert!(h8);
            }
        }
        (4, CmpKind::LtU | CmpKind::GeU) => {
            compare_swapped!(cmhi, s4);
            if matches!(kind, CmpKind::GeU) {
                invert!(s4);
            }
        }
        (1, CmpKind::GtU | CmpKind::LeU) => {
            compare!(cmhi, b16);
            if matches!(kind, CmpKind::LeU) {
                invert!(b16);
            }
        }
        (2, CmpKind::GtU | CmpKind::LeU) => {
            compare!(cmhi, h8);
            if matches!(kind, CmpKind::LeU) {
                invert!(h8);
            }
        }
        (4, CmpKind::GtU | CmpKind::LeU) => {
            compare!(cmhi, s4);
            if matches!(kind, CmpKind::LeU) {
                invert!(s4);
            }
        }
        (1, CmpKind::LtS | CmpKind::GeS) => {
            compare_swapped!(cmgt, b16);
            if matches!(kind, CmpKind::GeS) {
                invert!(b16);
            }
        }
        (2, CmpKind::LtS | CmpKind::GeS) => {
            compare_swapped!(cmgt, h8);
            if matches!(kind, CmpKind::GeS) {
                invert!(h8);
            }
        }
        (4, CmpKind::LtS | CmpKind::GeS) => {
            compare_swapped!(cmgt, s4);
            if matches!(kind, CmpKind::GeS) {
                invert!(s4);
            }
        }
        (1, CmpKind::GtS | CmpKind::LeS) => {
            compare!(cmgt, b16);
            if matches!(kind, CmpKind::LeS) {
                invert!(b16);
            }
        }
        (2, CmpKind::GtS | CmpKind::LeS) => {
            compare!(cmgt, h8);
            if matches!(kind, CmpKind::LeS) {
                invert!(h8);
            }
        }
        (4, CmpKind::GtS | CmpKind::LeS) => {
            compare!(cmgt, s4);
            if matches!(kind, CmpKind::LeS) {
                invert!(s4);
            }
        }
        _ => unreachable!(),
    }
}

fn emit_packed_vector_mask(ops: &mut VecAssembler<Aarch64Relocation>, element_stride: u8) {
    match element_stride {
        1 => {
            emit_load_imm(ops, SCRATCH1, 0x8040_2010_0804_0201);
            dynasm!(ops
                ; .arch aarch64
                ; ushr v0.b16, v0.b16, #7
                ; fmov d6, x17
                ; dup v6.d2, v6.d[0]
                ; mul v0.b16, v0.b16, v6.b16
                ; addv b5, v0.b8
                ; umov W(SCRATCH0), v5.b[0]
                ; ext v7.b16, v0.b16, v0.b16, #8
                ; addv b5, v7.b8
                ; umov W(SCRATCH1), v5.b[0]
                ; lsl x17, x17, #8
                ; orr x16, x16, x17
            );
        }
        2 => {
            emit_load_imm(ops, SCRATCH1, 0x0008_0004_0002_0001);
            dynasm!(ops ; .arch aarch64 ; fmov d6, x17);
            emit_load_imm(ops, SCRATCH1, 0x0080_0040_0020_0010);
            dynasm!(ops
                ; .arch aarch64
                ; fmov d7, x17
                ; ins v6.d[1], v7.d[0]
                ; ushr v0.h8, v0.h8, #15
                ; mul v0.h8, v0.h8, v6.h8
                ; addv h5, v0.h8
                ; umov W(SCRATCH0), v5.h[0]
            );
        }
        4 => {
            emit_load_imm(ops, SCRATCH1, 0x0000_0002_0000_0001);
            dynasm!(ops ; .arch aarch64 ; fmov d6, x17);
            emit_load_imm(ops, SCRATCH1, 0x0000_0008_0000_0004);
            dynasm!(ops
                ; .arch aarch64
                ; fmov d7, x17
                ; ins v6.d[1], v7.d[0]
                ; ushr v0.s4, v0.s4, #31
                ; mul v0.s4, v0.s4, v6.s4
                ; addv s5, v0.s4
                ; umov W(SCRATCH0), v5.s[0]
            );
        }
        _ => unreachable!(),
    }
}

fn emit_packed_byte_affine_compare(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    rhs: u8,
    kind: CmpKind,
) {
    // This pseudo always compares exactly sixteen byte lanes.  Keep the
    // affine sequence in NEON registers, then turn each 0xff/0x00 predicate
    // into one bit with a byte-weighted horizontal sum.  The scalar fallback
    // used to materialize and compare every lane independently.
    emit_load_imm(ops, 30, 0x0706_0504_0302_0100);
    dynasm!(ops ; .arch aarch64 ; fmov d2, x30);
    emit_load_imm(ops, 30, 0x0f0e_0d0c_0b0a_0908);
    dynasm!(ops
        ; .arch aarch64
        ; fmov d3, x30
        ; ins v2.d[1], v3.d[0]
        ; dup v4.b16, W(base)
        ; dup v1.b16, W(rhs)
        ; add v0.b16, v2.b16, v4.b16
        ; movi v5.b16, #0
    );
    match kind {
        CmpKind::Eq => {
            dynasm!(ops ; .arch aarch64 ; cmeq v0.b16, v0.b16, v1.b16);
        }
        CmpKind::Ne => {
            dynasm!(ops
                ; .arch aarch64
                ; cmeq v0.b16, v0.b16, v1.b16
                ; mvn v0.b16, v0.b16
            );
        }
        CmpKind::LtU | CmpKind::GeU => {
            dynasm!(ops
                ; .arch aarch64
                ; uqsub v2.b16, v1.b16, v0.b16
                ; cmeq v0.b16, v2.b16, v5.b16
            );
            if matches!(kind, CmpKind::LtU) {
                dynasm!(ops ; .arch aarch64 ; mvn v0.b16, v0.b16);
            }
        }
        CmpKind::GtU | CmpKind::LeU => {
            dynasm!(ops
                ; .arch aarch64
                ; uqsub v2.b16, v0.b16, v1.b16
                ; cmeq v0.b16, v2.b16, v5.b16
            );
            if matches!(kind, CmpKind::GtU) {
                dynasm!(ops ; .arch aarch64 ; mvn v0.b16, v0.b16);
            }
        }
        CmpKind::LtS | CmpKind::GeS => {
            dynasm!(ops ; .arch aarch64 ; cmgt v0.b16, v1.b16, v0.b16);
            if matches!(kind, CmpKind::GeS) {
                dynasm!(ops ; .arch aarch64 ; mvn v0.b16, v0.b16);
            }
        }
        CmpKind::GtS | CmpKind::LeS => {
            dynasm!(ops ; .arch aarch64 ; cmgt v0.b16, v0.b16, v1.b16);
            if matches!(kind, CmpKind::LeS) {
                dynasm!(ops ; .arch aarch64 ; mvn v0.b16, v0.b16);
            }
        }
    }
    emit_load_imm(ops, 30, 0x8040_2010_0804_0201);
    dynasm!(ops
        ; .arch aarch64
        ; ushr v0.b16, v0.b16, #7
        ; fmov d6, x30
        ; dup v6.d2, v6.d[0]
        ; mul v0.b16, v0.b16, v6.b16
        ; addv b5, v0.b8
        ; umov W(SCRATCH0), v5.b[0]
        ; ext v7.b16, v0.b16, v0.b16, #8
        ; addv b5, v7.b8
        ; umov W(SCRATCH1), v5.b[0]
        ; lsl x17, x17, #8
        ; orr x30, x16, x17
        ; mov X(destination), x30
    );
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
    state_pages: StatePageBases,
) {
    if byte_len == 0 {
        return;
    }
    let pattern = u64::from(value) * 0x0101_0101_0101_0101;
    let (base, offset) =
        select_vector_memory_base(BaseReg::SimState, i64::from(dst_offset), state_pages);
    emit_address_to(ops, SCRATCH0, base, offset);
    emit_load_imm(ops, 30, pattern);
    let vector_chunks = byte_len / 16;
    if vector_chunks != 0 {
        dynasm!(ops
            ; .arch aarch64
            ; fmov d0, x30
            ; dup v0.d2, v0.d[0]
        );
        if vector_chunks <= 32 {
            for _ in 0..vector_chunks {
                dynasm!(ops ; .arch aarch64 ; str q0, [x16], #16);
            }
        } else {
            let loop_label = ops.new_dynamic_label();
            let done = ops.new_dynamic_label();
            emit_load_imm(ops, SCRATCH1, vector_chunks as u64);
            dynasm!(ops
                ; .arch aarch64
                ; =>loop_label
                ; cbz x17, =>done
                ; str q0, [x16], #16
                ; sub x17, x17, #1
                ; b =>loop_label
                ; =>done
            );
        }
    }
    let remainder = byte_len % 16;
    if remainder >= 8 {
        dynasm!(ops ; .arch aarch64 ; str x30, [x16], #8);
    }
    if remainder % 8 >= 4 {
        dynasm!(ops ; .arch aarch64 ; str w30, [x16], #4);
    }
    if remainder % 4 >= 2 {
        dynasm!(ops ; .arch aarch64 ; strh w30, [x16], #2);
    }
    if remainder % 2 == 1 {
        dynasm!(ops ; .arch aarch64 ; strb w30, [x16]);
    }
}

fn select_state_base_pages(
    function: &MFunction,
    secondary_registers: impl IntoIterator<Item = u8>,
) -> StatePageBases {
    let accesses = collect_state_page_accesses(function);
    let candidates = accesses
        .iter()
        .map(|access| access.offset().div_euclid(STATE_PAGE_BYTES) * STATE_PAGE_BYTES)
        .collect::<BTreeSet<_>>();
    let mut selected = Vec::new();
    let mut current_costs = accesses
        .iter()
        .copied()
        .map(state_page_baseline_cost)
        .collect::<Vec<_>>();

    // Greedily choose pages by the actual number of address-materialization
    // instructions they remove.  A page base can cover several adjacent
    // pages for wider accesses, so counting exact pages independently can
    // waste preserved registers on redundant bases.
    for _ in 0..=MAX_SECONDARY_STATE_PAGES {
        let mut best = None;
        for &candidate in &candidates {
            if selected.contains(&candidate) {
                continue;
            }
            let benefit = accesses
                .iter()
                .copied()
                .zip(current_costs.iter().copied())
                .map(|(access, current_cost)| {
                    state_page_cost(access, candidate)
                        .map_or(0, |cost| current_cost.saturating_sub(cost))
                })
                .sum::<usize>();
            if best
                .map(|(best_benefit, best_page)| {
                    benefit > best_benefit || (benefit == best_benefit && candidate < best_page)
                })
                .unwrap_or(true)
            {
                best = Some((benefit, candidate));
            }
        }
        let Some((benefit, page)) = best else {
            break;
        };
        if benefit < MIN_STATE_PAGE_BENEFIT {
            break;
        }
        selected.push(page);
        for (access, current_cost) in accesses.iter().copied().zip(&mut current_costs) {
            if let Some(cost) = state_page_cost(access, page) {
                *current_cost = (*current_cost).min(cost);
            }
        }
    }

    let secondary = secondary_registers
        .into_iter()
        .zip(selected.iter().skip(1))
        .take(MAX_SECONDARY_STATE_PAGES)
        .map(|(register, page)| Some((register, *page)))
        .chain(std::iter::repeat(None))
        .take(MAX_SECONDARY_STATE_PAGES)
        .collect::<Vec<_>>()
        .try_into()
        .expect("secondary state page count is fixed");
    StatePageBases {
        primary: selected.first().copied(),
        secondary,
    }
}

fn collect_state_page_accesses(function: &MFunction) -> Vec<StatePageAccess> {
    let mut accesses = Vec::new();
    for block in &function.blocks {
        for instruction in &block.insts {
            match instruction {
                MInst::Load {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => accesses.push(StatePageAccess::Direct {
                    offset: i64::from(*offset),
                    size: *size,
                    store: false,
                }),
                MInst::Store {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => accesses.push(StatePageAccess::Direct {
                    offset: i64::from(*offset),
                    size: *size,
                    store: true,
                }),
                MInst::AndStoreImm {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                }
                | MInst::OrStoreImm {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => {
                    let offset = i64::from(*offset);
                    accesses.push(StatePageAccess::Direct {
                        offset,
                        size: *size,
                        store: false,
                    });
                    accesses.push(StatePageAccess::Direct {
                        offset,
                        size: *size,
                        store: true,
                    });
                }
                MInst::LoadIndexed {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => accesses.push(StatePageAccess::Indexed {
                    offset: i64::from(*offset),
                    size: *size,
                    store: false,
                }),
                MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => accesses.push(StatePageAccess::Indexed {
                    offset: i64::from(*offset),
                    size: *size,
                    store: true,
                }),
                MInst::OrStoreIndexed {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => {
                    let offset = i64::from(*offset);
                    accesses.push(StatePageAccess::Indexed {
                        offset,
                        size: *size,
                        store: false,
                    });
                    accesses.push(StatePageAccess::Indexed {
                        offset,
                        size: *size,
                        store: true,
                    });
                }
                MInst::MemCopy {
                    src_offset,
                    dst_offset,
                    byte_len,
                } if mem_copy_uses_forward_vectors(*src_offset, *dst_offset, *byte_len) => {
                    accesses.push(StatePageAccess::Vector {
                        offset: i64::from(*src_offset),
                    });
                    accesses.push(StatePageAccess::Vector {
                        offset: i64::from(*dst_offset),
                    });
                }
                MInst::MemFill { dst_offset, .. } => {
                    accesses.push(StatePageAccess::Vector {
                        offset: i64::from(*dst_offset),
                    });
                }
                MInst::BranchPred {
                    predicate:
                        BranchPredicate::MemoryNonZero {
                            base: BaseReg::SimState,
                            offset,
                            size,
                        },
                    ..
                } => accesses.push(StatePageAccess::Direct {
                    offset: i64::from(*offset),
                    size: *size,
                    store: false,
                }),
                MInst::SparseMarkActive {
                    active_index,
                    active_bits_offset,
                    ..
                } => {
                    let offset = i64::from(*active_bits_offset) + i64::from(*active_index / 64) * 8;
                    accesses.push(StatePageAccess::Direct {
                        offset,
                        size: OpSize::S64,
                        store: false,
                    });
                    accesses.push(StatePageAccess::Direct {
                        offset,
                        size: OpSize::S64,
                        store: true,
                    });
                }
                MInst::PackedLaneCompare {
                    offset,
                    lane_count,
                    element_stride,
                    rhs,
                    ..
                } => collect_packed_lane_accesses(
                    &mut accesses,
                    *offset,
                    *lane_count,
                    *element_stride,
                    *rhs,
                ),
                _ => {}
            }
        }
    }
    accesses
}

fn collect_packed_lane_accesses(
    accesses: &mut Vec<StatePageAccess>,
    offset: i32,
    lane_count: u8,
    element_stride: u8,
    rhs: PackedLaneCompareRhs,
) {
    let Some(size) = packed_lane_size(element_stride) else {
        return;
    };
    let lane_count = usize::from(lane_count);
    let element_stride = usize::from(element_stride);
    if lane_count * element_stride % 16 == 0 {
        let lanes_per_vector = 16 / element_stride;
        for lane_base in (0..lane_count).step_by(lanes_per_vector) {
            let delta =
                i64::try_from(lane_base * element_stride).expect("packed lane offset fits in i64");
            accesses.push(StatePageAccess::Vector {
                offset: i64::from(offset) + delta,
            });
            if let PackedLaneCompareRhs::Memory {
                offset: rhs_offset, ..
            } = rhs
            {
                accesses.push(StatePageAccess::Vector {
                    offset: i64::from(rhs_offset) + delta,
                });
            }
        }
    } else {
        for lane in 0..lane_count {
            let delta =
                i64::try_from(lane * element_stride).expect("packed lane offset fits in i64");
            accesses.push(StatePageAccess::Direct {
                offset: i64::from(offset) + delta,
                size,
                store: false,
            });
            if let PackedLaneCompareRhs::Memory {
                offset: rhs_offset, ..
            } = rhs
            {
                accesses.push(StatePageAccess::Direct {
                    offset: i64::from(rhs_offset) + delta,
                    size,
                    store: false,
                });
            }
        }
    }
}

fn packed_lane_size(element_stride: u8) -> Option<OpSize> {
    match element_stride {
        1 => Some(OpSize::S8),
        2 => Some(OpSize::S16),
        4 => Some(OpSize::S32),
        _ => None,
    }
}

fn mem_copy_uses_forward_vectors(src_offset: i32, dst_offset: i32, byte_len: usize) -> bool {
    if byte_len == 0 || src_offset == dst_offset {
        return false;
    }
    let byte_len = byte_len as i64;
    let src_end = i64::from(src_offset) + byte_len;
    let dst_end = i64::from(dst_offset) + byte_len;
    let copy_backward = src_end > i64::from(dst_offset)
        && dst_end > i64::from(src_offset)
        && dst_offset > src_offset;
    !copy_backward && (16..=256).contains(&byte_len)
}

fn state_page_baseline_cost(access: StatePageAccess) -> usize {
    match access {
        StatePageAccess::Direct {
            offset,
            size,
            store,
        } => {
            if memory_access_encoding(SCRATCH0, STATE_REG, offset, size, store).is_some() {
                0
            } else {
                address_materialization_cost(offset)
            }
        }
        StatePageAccess::Indexed {
            offset,
            size,
            store,
        } => indexed_access_cost(offset, size, store),
        StatePageAccess::Vector { offset } => address_materialization_cost(offset),
    }
}

fn state_page_cost(access: StatePageAccess, page: i64) -> Option<usize> {
    let offset = access.offset() - page;
    match access {
        StatePageAccess::Direct { size, store, .. } => {
            memory_access_encoding(SCRATCH0, STATE_PAGE_REG, offset, size, store).map(|_| 0)
        }
        StatePageAccess::Indexed { size, store, .. } => indexed_offset_cost(offset, size, store),
        StatePageAccess::Vector { .. } => Some(address_materialization_cost(offset)),
    }
}

fn indexed_access_cost(offset: i64, size: OpSize, store: bool) -> usize {
    indexed_offset_cost(offset, size, store).unwrap_or_else(|| address_materialization_cost(offset))
}

fn indexed_offset_cost(offset: i64, size: OpSize, store: bool) -> Option<usize> {
    if memory_access_encoding(SCRATCH0, SCRATCH0, offset, size, store).is_some() {
        Some(0)
    } else if add_sub_immediate(offset).is_some() {
        Some(1)
    } else if add_sub_immediate_pair(offset).is_some() {
        Some(2)
    } else {
        None
    }
}

fn select_memory_base(
    base: BaseReg,
    offset: i64,
    register: u8,
    size: OpSize,
    store: bool,
    state_pages: StatePageBases,
) -> (u8, i64) {
    let normal = (base_register(base), offset);
    if base != BaseReg::SimState {
        return normal;
    }
    if let Some(page) = state_pages.primary {
        let relative = offset - page;
        if memory_access_encoding(register, STATE_PAGE_REG, relative, size, store).is_some() {
            return (STATE_PAGE_REG, relative);
        }
    }
    for &(page_register, page) in state_pages.secondary.iter().flatten() {
        let relative = offset - page;
        if memory_access_encoding(register, page_register, relative, size, store).is_some() {
            return (page_register, relative);
        }
    }
    normal
}

fn select_indexed_memory_base(
    base: BaseReg,
    offset: i64,
    size: OpSize,
    store: bool,
    state_pages: StatePageBases,
) -> (u8, i64) {
    let normal = (base_register(base), offset);
    if base != BaseReg::SimState {
        return normal;
    }
    let candidates = std::iter::once((indexed_access_cost(offset, size, store), normal)).chain(
        state_pages
            .primary
            .into_iter()
            .map(|page| (STATE_PAGE_REG, page))
            .chain(state_pages.secondary.iter().flatten().copied())
            .filter_map(|(page_register, page)| {
                let relative = offset - page;
                indexed_offset_cost(relative, size, store)
                    .map(|cost| (cost, (page_register, relative)))
            }),
    );
    candidates
        .min_by_key(|(cost, _)| *cost)
        .map(|(_, base)| base)
        .unwrap_or(normal)
}

fn select_vector_memory_base(base: BaseReg, offset: i64, state_pages: StatePageBases) -> (u8, i64) {
    let normal = (base_register(base), offset);
    if base != BaseReg::SimState {
        return normal;
    }
    let candidates = std::iter::once((address_materialization_cost(offset), normal)).chain(
        state_pages
            .primary
            .into_iter()
            .map(|page| (STATE_PAGE_REG, page))
            .chain(state_pages.secondary.iter().flatten().copied())
            .map(|(page_register, page)| {
                let relative = offset - page;
                (
                    address_materialization_cost(relative),
                    (page_register, relative),
                )
            }),
    );
    candidates
        .min_by_key(|(cost, _)| *cost)
        .map(|(_, base)| base)
        .unwrap_or(normal)
}

fn address_materialization_cost(offset: i64) -> usize {
    if offset == 0 {
        0
    } else if add_sub_immediate(offset).is_some() {
        1
    } else if add_sub_immediate_pair(offset).is_some() {
        2
    } else {
        move_wide_plan(offset as u64).instruction_count + 1
    }
}

fn base_register(base: BaseReg) -> u8 {
    match base {
        BaseReg::SimState => STATE_REG,
        BaseReg::StackFrame => SPILL_REG,
    }
}

fn emit_base_address(
    ops: &mut VecAssembler<Aarch64Relocation>,
    base: BaseReg,
    offset: i32,
) -> Result<(), EmitError> {
    let offset = base_offset(base, offset);
    emit_address(ops, base_register(base), offset);
    Ok(())
}

fn base_offset(base: BaseReg, offset: i32) -> i64 {
    match base {
        BaseReg::SimState | BaseReg::StackFrame => i64::from(offset),
    }
}

fn emit_address(ops: &mut VecAssembler<Aarch64Relocation>, base: u8, offset: i64) {
    emit_address_to(ops, SCRATCH0, base, offset);
}

fn emit_address_to(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    offset: i64,
) {
    if offset == 0 {
        dynasm!(ops ; .arch aarch64 ; mov X(destination), X(base));
    } else if !emit_add_sub_immediate(ops, destination, base, offset) {
        if let Some((high, low)) = add_sub_immediate_pair(offset) {
            let _ = emit_add_sub_immediate(ops, destination, base, high);
            let _ = emit_add_sub_immediate(ops, destination, destination, low);
        } else {
            emit_load_imm(ops, SCRATCH1, offset as u64);
            dynasm!(ops ; .arch aarch64 ; add X(destination), X(base), x17);
        }
    }
}

fn memory_access_encoding(
    register: u8,
    base: u8,
    offset: i64,
    size: OpSize,
    store: bool,
) -> Option<u32> {
    let (bytes, scaled_opcode, unscaled_opcode) = match (size, store) {
        (OpSize::S8, false) => (1, 0x3940_0000, 0x3840_0000),
        (OpSize::S8, true) => (1, 0x3900_0000, 0x3800_0000),
        (OpSize::S16, false) => (2, 0x7940_0000, 0x7840_0000),
        (OpSize::S16, true) => (2, 0x7900_0000, 0x7800_0000),
        (OpSize::S32, false) => (4, 0xb940_0000, 0xb840_0000),
        (OpSize::S32, true) => (4, 0xb900_0000, 0xb800_0000),
        (OpSize::S64, false) => (8, 0xf940_0000, 0xf840_0000),
        (OpSize::S64, true) => (8, 0xf900_0000, 0xf800_0000),
    };
    let register = u32::from(register);
    let base = u32::from(base);
    let registers = (base << 5) | register;

    if offset >= 0 && offset % bytes == 0 {
        let scaled = offset / bytes;
        if scaled <= 0xfff {
            return Some(scaled_opcode | ((scaled as u32) << 10) | registers);
        }
    }
    if (-256..=255).contains(&offset) {
        let immediate = ((offset as i32) & 0x1ff) as u32;
        return Some(unscaled_opcode | (immediate << 12) | registers);
    }
    None
}

fn emit_load_at(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    offset: i64,
    size: OpSize,
) {
    if let Some(instruction) = memory_access_encoding(destination, base, offset, size, false) {
        ops.push_u32(instruction);
    } else {
        emit_address(ops, base, offset);
        emit_load(ops, destination, SCRATCH0, size);
    }
}

fn emit_load_indexed_at(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    index: u8,
    offset: i64,
    scale: u8,
    size: OpSize,
) {
    if offset == 0
        && let Some(shift) = indexed_address_shift(scale, size)
    {
        match size {
            OpSize::S8 => {
                dynasm!(ops ; .arch aarch64 ; ldrb W(destination), [X(base), X(index), LSL #shift])
            }
            OpSize::S16 => {
                dynasm!(ops ; .arch aarch64 ; ldrh W(destination), [X(base), X(index), LSL #shift])
            }
            OpSize::S32 => {
                dynasm!(ops ; .arch aarch64 ; ldr W(destination), [X(base), X(index), LSL #shift])
            }
            OpSize::S64 => {
                dynasm!(ops ; .arch aarch64 ; ldr X(destination), [X(base), X(index), LSL #shift])
            }
        }
    } else {
        let shift = scale.trailing_zeros();
        dynasm!(ops ; .arch aarch64 ; add x16, X(base), X(index), LSL #shift);
        emit_load_at(ops, destination, SCRATCH0, offset, size);
    }
}

fn emit_store_at(
    ops: &mut VecAssembler<Aarch64Relocation>,
    source: u8,
    base: u8,
    offset: i64,
    size: OpSize,
) {
    if let Some(instruction) = memory_access_encoding(source, base, offset, size, true) {
        ops.push_u32(instruction);
    } else {
        emit_address(ops, base, offset);
        emit_store(ops, source, SCRATCH0, size);
    }
}

fn emit_store_indexed_at(
    ops: &mut VecAssembler<Aarch64Relocation>,
    source: u8,
    base: u8,
    index: u8,
    offset: i64,
    scale: u8,
    size: OpSize,
) {
    if offset == 0
        && let Some(shift) = indexed_address_shift(scale, size)
    {
        match size {
            OpSize::S8 => {
                dynasm!(ops ; .arch aarch64 ; strb W(source), [X(base), X(index), LSL #shift])
            }
            OpSize::S16 => {
                dynasm!(ops ; .arch aarch64 ; strh W(source), [X(base), X(index), LSL #shift])
            }
            OpSize::S32 => {
                dynasm!(ops ; .arch aarch64 ; str W(source), [X(base), X(index), LSL #shift])
            }
            OpSize::S64 => {
                dynasm!(ops ; .arch aarch64 ; str X(source), [X(base), X(index), LSL #shift])
            }
        }
    } else {
        let shift = scale.trailing_zeros();
        dynasm!(ops ; .arch aarch64 ; add x16, X(base), X(index), LSL #shift);
        emit_store_at(ops, source, SCRATCH0, offset, size);
    }
}

fn indexed_address_shift(scale: u8, size: OpSize) -> Option<u32> {
    let natural_scale = match size {
        OpSize::S8 => 1,
        OpSize::S16 => 2,
        OpSize::S32 => 4,
        OpSize::S64 => 8,
    };
    if scale == 1 {
        Some(0)
    } else if scale == natural_scale {
        Some(scale.trailing_zeros())
    } else {
        None
    }
}

fn emit_add_offset(ops: &mut VecAssembler<Aarch64Relocation>, offset: i64) {
    if offset != 0 {
        if !emit_add_sub_immediate(ops, SCRATCH0, SCRATCH0, offset) {
            if let Some((high, low)) = add_sub_immediate_pair(offset) {
                let _ = emit_add_sub_immediate(ops, SCRATCH0, SCRATCH0, high);
                let _ = emit_add_sub_immediate(ops, SCRATCH0, SCRATCH0, low);
            } else {
                emit_load_imm(ops, SCRATCH1, offset as u64);
                dynasm!(ops ; .arch aarch64 ; add x16, x16, x17);
            }
        }
    }
}

fn add_sub_immediate(offset: i64) -> Option<(bool, u32, bool)> {
    let (subtract, magnitude) = if offset < 0 {
        (true, offset.unsigned_abs())
    } else {
        (false, offset as u64)
    };
    if magnitude <= 0xfff {
        return Some((subtract, magnitude as u32, false));
    }
    if magnitude.is_multiple_of(0x1000) && magnitude / 0x1000 <= 0xfff {
        return Some((subtract, (magnitude / 0x1000) as u32, true));
    }
    None
}

fn add_sub_immediate_pair(offset: i64) -> Option<(i64, i64)> {
    let magnitude = offset.unsigned_abs();
    let high = magnitude & !0xfff;
    let low = magnitude & 0xfff;
    if high == 0 || low == 0 {
        return None;
    }
    let sign = if offset < 0 { -1_i64 } else { 1_i64 };
    let high = i64::try_from(high).ok()?.checked_mul(sign)?;
    let low = i64::try_from(low).ok()?.checked_mul(sign)?;
    (add_sub_immediate(high).is_some() && add_sub_immediate(low).is_some()).then_some((high, low))
}

fn add_sub_immediate_encoding(
    destination: u8,
    base: u8,
    offset: i64,
    set_flags: bool,
) -> Option<u32> {
    let (subtract, immediate, shifted) = add_sub_immediate(offset)?;
    // ADD/SUB (immediate): sf=1, op selects SUB, S selects flag-setting,
    // and sh selects an optional <<12 immediate.  Using the same encoder for
    // arithmetic and CMP keeps the immediate selection identical.
    let opcode = if set_flags { 0xb100_0000 } else { 0x9100_0000 };
    let instruction = opcode
        | (u32::from(subtract) << 30)
        | (u32::from(shifted) << 22)
        | (immediate << 10)
        | (u32::from(base) << 5)
        | u32::from(destination);
    debug_assert!(immediate <= 0xfff);
    Some(instruction)
}

fn emit_add_sub_immediate(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    offset: i64,
) -> bool {
    let Some(instruction) = add_sub_immediate_encoding(destination, base, offset, false) else {
        return false;
    };
    ops.push_u32(instruction);
    true
}

fn emit_cmp_immediate(ops: &mut VecAssembler<Aarch64Relocation>, lhs: u8, immediate: i32) -> bool {
    // CMP lhs, #imm is SUBS lhs, #imm.  A negative immediate is therefore
    // represented by ADDS with its positive magnitude.  The immediate is
    // sign-extended to 64 bits by the MIR contract.
    let Some(instruction) = add_sub_immediate_encoding(31, lhs, -i64::from(immediate), true) else {
        return false;
    };
    ops.push_u32(instruction);
    true
}

fn logical_immediate_encoding(
    value: u64,
    width: u32,
    destination: u8,
    source: u8,
    is_and: bool,
) -> Option<u32> {
    debug_assert!(matches!(width, 32 | 64));
    let width_mask = if width == 32 {
        u64::from(u32::MAX)
    } else {
        u64::MAX
    };
    let value = value & width_mask;
    let opcode = match (width, is_and) {
        (32, true) => 0x1200_0000,
        (32, false) => 0x3200_0000,
        (64, true) => 0x9200_0000,
        (64, false) => 0xb200_0000,
        _ => unreachable!(),
    };

    // AArch64 represents a logical immediate as a rotated run of ones in a
    // power-of-two element, replicated to the operand width.  Try the
    // smallest element first; this also selects N=0 for repeated masks and
    // N=1 only for a full 64-bit element.
    for element_size in [2_u32, 4, 8, 16, 32, 64] {
        if element_size > width {
            continue;
        }
        let element_mask = if element_size == 64 {
            u64::MAX
        } else {
            (1_u64 << element_size) - 1
        };
        let pattern = value & element_mask;
        if pattern == 0 || pattern == element_mask {
            continue;
        }
        let mut repeated = 0_u64;
        let mut offset = 0_u32;
        while offset < width {
            repeated |= pattern << offset;
            offset += element_size;
        }
        if repeated != value {
            continue;
        }

        let ones = pattern.count_ones();
        let length = element_size.trailing_zeros();
        let imms_prefix = (!((1_u32 << (length + 1)) - 1)) & 0x3f;
        let imms = imms_prefix | (ones - 1);
        let run = (1_u64 << ones) - 1;
        for rotation in 0..element_size {
            if rotate_right(run, rotation, element_size) != pattern {
                continue;
            }
            let n = u32::from(width == 64 && element_size == 64);
            return Some(
                opcode
                    | (n << 22)
                    | (rotation << 16)
                    | (imms << 10)
                    | (u32::from(source) << 5)
                    | u32::from(destination),
            );
        }
    }
    None
}

fn rotate_right(value: u64, amount: u32, width: u32) -> u64 {
    let amount = amount % width;
    let mask = if width == 64 {
        u64::MAX
    } else {
        (1_u64 << width) - 1
    };
    if amount == 0 {
        value & mask
    } else {
        ((value >> amount) | (value << (width - amount))) & mask
    }
}

fn emit_logical_immediate(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    source: u8,
    value: u64,
    width: u32,
    is_and: bool,
) -> bool {
    let Some(instruction) = logical_immediate_encoding(value, width, destination, source, is_and)
    else {
        return false;
    };
    ops.push_u32(instruction);
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MoveWidePlan {
    inverted: bool,
    first: usize,
    instruction_count: usize,
}

fn move_wide_plan(value: u64) -> MoveWidePlan {
    let halves = [
        value as u16,
        (value >> 16) as u16,
        (value >> 32) as u16,
        (value >> 48) as u16,
    ];
    let movz_candidates = halves
        .iter()
        .enumerate()
        .filter_map(|(index, &half)| (half != 0).then_some(index))
        .collect::<Vec<_>>();
    let movn_candidates = halves
        .iter()
        .enumerate()
        .filter_map(|(index, &half)| (half != u16::MAX).then_some(index))
        .collect::<Vec<_>>();
    let movz = MoveWidePlan {
        inverted: false,
        first: movz_candidates.first().copied().unwrap_or(0),
        instruction_count: movz_candidates.len().max(1),
    };
    let movn = MoveWidePlan {
        inverted: true,
        first: movn_candidates.first().copied().unwrap_or(0),
        instruction_count: movn_candidates.len().max(1),
    };
    if movn.instruction_count < movz.instruction_count {
        movn
    } else {
        movz
    }
}

fn emit_load_imm(ops: &mut VecAssembler<Aarch64Relocation>, register: u8, value: u64) {
    let halves = [
        value as u16,
        (value >> 16) as u16,
        (value >> 32) as u16,
        (value >> 48) as u16,
    ];
    let plan = move_wide_plan(value);
    let first = plan.first;
    let fill = if plan.inverted { u16::MAX } else { 0 };
    let half = if plan.inverted {
        u32::from(!halves[first])
    } else {
        u32::from(halves[first])
    };
    match (plan.inverted, first) {
        (false, 0) => dynasm!(ops ; .arch aarch64 ; movz X(register), half),
        (false, 1) => dynasm!(ops ; .arch aarch64 ; movz X(register), half, LSL #16),
        (false, 2) => dynasm!(ops ; .arch aarch64 ; movz X(register), half, LSL #32),
        (false, 3) => dynasm!(ops ; .arch aarch64 ; movz X(register), half, LSL #48),
        (true, 0) => dynasm!(ops ; .arch aarch64 ; movn X(register), half),
        (true, 1) => dynasm!(ops ; .arch aarch64 ; movn X(register), half, LSL #16),
        (true, 2) => dynasm!(ops ; .arch aarch64 ; movn X(register), half, LSL #32),
        (true, 3) => dynasm!(ops ; .arch aarch64 ; movn X(register), half, LSL #48),
        _ => unreachable!(),
    }
    for (index, half) in halves.into_iter().enumerate() {
        if index == first || half == fill {
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
    state_pages: StatePageBases,
) -> Result<(), EmitError> {
    match predicate {
        BranchPredicate::Compare { lhs, rhs, .. } => {
            let (lhs, rhs) = (resolve(assignment, lhs)?, resolve(assignment, rhs)?);
            dynasm!(ops ; .arch aarch64 ; cmp X(lhs), X(rhs));
        }
        BranchPredicate::CompareImm { lhs, imm, .. } => {
            let lhs = resolve(assignment, lhs)?;
            if !emit_cmp_immediate(ops, lhs, imm) {
                emit_load_imm(ops, SCRATCH0, imm as i64 as u64);
                dynasm!(ops ; .arch aarch64 ; cmp X(lhs), x16);
            }
        }
        BranchPredicate::MemoryNonZero { base, offset, size } => {
            let offset = base_offset(base, offset);
            let (base_register, offset) =
                select_memory_base(base, offset, SCRATCH0, size, false, state_pages);
            emit_load_at(ops, SCRATCH0, base_register, offset, size);
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
            } => emit_copy(ops, destination, source)?,
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
                read_copy_destination(ops, destination, SCRATCH1)?;
                // Address materialization uses x17 for the offset. Preserve
                // the value being saved before computing the temporary slot.
                dynasm!(ops ; .arch aarch64 ; mov x30, x17);
                let temporary = temporary_offset
                    .checked_sub(spill_base)
                    .ok_or(EmitError::Range("temporary spill offset underflow"))?;
                emit_store_at(
                    ops,
                    30,
                    SPILL_REG,
                    i64::try_from(temporary)
                        .map_err(|_| EmitError::Range("temporary spill offset overflow"))?,
                    OpSize::S64,
                );
            }
            CopyOperation::RestoreTemporary(destination) => {
                let temporary = temporary_offset
                    .checked_sub(spill_base)
                    .ok_or(EmitError::Range("temporary spill offset underflow"))?;
                emit_load_at(
                    ops,
                    SCRATCH1,
                    SPILL_REG,
                    i64::try_from(temporary)
                        .map_err(|_| EmitError::Range("temporary spill offset overflow"))?,
                    OpSize::S64,
                );
                write_copy_destination(ops, destination, SCRATCH1)?;
            }
        }
    }
    Ok(())
}

fn emit_copy(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: CopyDestination,
    source: CopySource,
) -> Result<(), EmitError> {
    match source {
        CopySource::Register(register) => {
            write_copy_destination(ops, destination, register.number())
        }
        CopySource::Stack(offset) => {
            emit_load_at(ops, SCRATCH1, SPILL_REG, i64::from(offset), OpSize::S64);
            write_copy_destination(ops, destination, SCRATCH1)
        }
        CopySource::Immediate(value) => {
            emit_load_imm(ops, SCRATCH1, value);
            write_copy_destination(ops, destination, SCRATCH1)
        }
    }
}

fn read_copy_destination(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: CopyDestination,
    output: u8,
) -> Result<(), EmitError> {
    match destination {
        CopyDestination::Register(register) => {
            let register = register.number();
            dynasm!(ops ; .arch aarch64 ; mov X(output), X(register));
        }
        CopyDestination::Stack(offset) => {
            emit_load_at(ops, output, SPILL_REG, i64::from(offset), OpSize::S64);
        }
    }
    Ok(())
}

fn write_copy_destination(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: CopyDestination,
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
            emit_store_at(ops, source, SPILL_REG, i64::from(offset), OpSize::S64);
        }
    }
    Ok(())
}

/// Hex-word fallback used in traces until an AArch64 disassembler is added.
pub fn disassemble(code: &[u8], base_addr: u64) -> String {
    code.as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            let word = u32::from_le_bytes(*bytes);
            format!("{:08x}: {word:08x}\n", base_addr + (index * 4) as u64)
        })
        .collect()
}
