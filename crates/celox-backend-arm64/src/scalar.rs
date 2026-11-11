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

#[cfg(test)]
mod function_call_tests;
#[cfg(all(test, target_arch = "aarch64"))]
mod tests;

mod arithmetic;
mod blocks;
mod control_flow;
mod extern_call;
mod instruction;
mod memory;
mod operands;
mod packed_compare;
mod state_pages;

pub(crate) use arithmetic::*;
use control_flow::*;
use instruction::*;
use memory::*;
use operands::*;
use packed_compare::*;
use state_pages::*;

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

// x29 holds a function unit's frame address, which has neither a tick loop
// nor a primary state page.
const FRAME_REG: u8 = STATE_PAGE_REG;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StatePageBases {
    primary: Option<i64>,
    secondary: [Option<(u8, i64)>; MAX_SECONDARY_STATE_PAGES],
    /// A function unit's frame window, addressed from [`FRAME_REG`].
    frame: Option<crate::function_unit::FrameWindow>,
}

impl StatePageBases {
    /// The frame register and offset that address state offset `offset`
    /// when it lies in a function unit's frame window.
    fn frame_base(self, base: BaseReg, offset: i64) -> Option<(u8, i64)> {
        let window = self.frame?;
        if base != BaseReg::SimState {
            return None;
        }
        let offset = i32::try_from(offset).ok()?;
        Some((FRAME_REG, i64::from(window.frame_offset(offset)?)))
    }
}

/// What the emitter of a function needs to know about function calls.
#[derive(Clone, Copy, Debug, Default)]
struct FunctionFrame {
    /// The frame window of a function unit: its frame address is in
    /// [`FRAME_REG`], and its spills and temporaries live on the machine
    /// stack, one copy per activation.
    window: Option<crate::function_unit::FrameWindow>,
    /// The call stack of the function's calls of function units.
    call_stack: Option<celox_state_layout::CallStackLayout>,
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
    /// The compile observed its cancellation token; not a backend defect.
    Cancelled,
}

impl fmt::Display for ChainedEmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prepare(error) => error.fmt(formatter),
            Self::Emit(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("compilation was cancelled"),
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
    trace: Option<&mut NativeFunctionTrace>,
    is_cancelled: impl Fn() -> bool,
) -> Result<EmitResult, ChainedEmitError> {
    emit_prepared_eu_with_arena(
        sir_eu,
        layout,
        four_state,
        label,
        native_tick_loop,
        None,
        trace,
        is_cancelled,
    )
}

/// Like [`emit_prepared_eu`], placing the function's private spill arena at
/// `arena_base` (at least the end of the semantic state) when given.
/// Functions that may run concurrently need disjoint arenas.
#[allow(clippy::too_many_arguments)]
pub fn emit_prepared_eu_with_arena(
    sir_eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    layout: &crate::MemoryLayout,
    four_state: bool,
    label: &str,
    native_tick_loop: bool,
    arena_base: Option<usize>,
    trace: Option<&mut NativeFunctionTrace>,
    is_cancelled: impl Fn() -> bool,
) -> Result<EmitResult, ChainedEmitError> {
    emit_prepared_eu_with_functions(
        sir_eu,
        layout,
        four_state,
        label,
        native_tick_loop,
        arena_base,
        None,
        trace,
        is_cancelled,
    )
}

/// Like [`emit_prepared_eu_with_arena`], for a unit of a program with the
/// function units `functions`, which may be one of them.
#[allow(clippy::too_many_arguments)]
pub fn emit_prepared_eu_with_functions(
    sir_eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    layout: &crate::MemoryLayout,
    four_state: bool,
    label: &str,
    native_tick_loop: bool,
    arena_base: Option<usize>,
    functions: Option<crate::FunctionCalls<'_>>,
    mut trace: Option<&mut NativeFunctionTrace>,
    is_cancelled: impl Fn() -> bool,
) -> Result<EmitResult, ChainedEmitError> {
    let allocated = allocate_prepared_eu_with_functions(
        sir_eu,
        layout,
        four_state,
        label,
        native_tick_loop,
        functions,
        trace.as_deref_mut(),
        &is_cancelled,
    )?;
    allocated.emit(arena_base, trace, is_cancelled)
}

/// Allocated AArch64 code whose private arena has not yet been placed.
///
/// Selection, optimization and allocation depend on the semantic layout, not
/// on the scratch extents of generated callees, so a caller can allocate
/// while its callees still compile and emit once their extents are known.
pub struct AllocatedFunction {
    allocation: crate::regalloc::TargetAllocation,
    semantic_size: usize,
    tick_loop: bool,
    check_runtime_events: bool,
    frame: FunctionFrame,
}

impl AllocatedFunction {
    /// Emit with the private arena at `arena_base` (at least the end of the
    /// semantic state) when given, as [`emit_prepared_eu_with_arena`] does.
    pub fn emit(
        self,
        arena_base: Option<usize>,
        trace: Option<&mut NativeFunctionTrace>,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<EmitResult, ChainedEmitError> {
        if is_cancelled() {
            return Err(ChainedEmitError::Cancelled);
        }
        let state_size = arena_base.map_or(self.semantic_size, |base| base.max(self.semantic_size));
        let result = emit_function(
            &self.allocation.allocated.function,
            &self.allocation.allocated.assignment,
            self.allocation.spill_frame_size,
            state_size,
            &self.allocation.allocated.edge_copies,
            self.tick_loop,
            self.check_runtime_events,
            self.frame,
        )?;
        if let Some(trace) = trace {
            trace.disassembly = disassemble(&result.code[..result.text_size], 0);
        }
        Ok(result)
    }
}

/// Select, optimize and allocate one prepared SIR execution unit, leaving
/// only emission, which places the private arena, to
/// [`AllocatedFunction::emit`].
#[allow(clippy::too_many_arguments)]
pub fn allocate_prepared_eu_with_functions(
    sir_eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    layout: &crate::MemoryLayout,
    four_state: bool,
    label: &str,
    native_tick_loop: bool,
    functions: Option<crate::FunctionCalls<'_>>,
    mut trace: Option<&mut NativeFunctionTrace>,
    is_cancelled: impl Fn() -> bool,
) -> Result<AllocatedFunction, ChainedEmitError> {
    // A function unit's slots become state objects in its frame window.
    let mut function_layout = None;
    let mut window = None;
    let mut prepared = None;
    if let Some(function) = functions.and_then(|functions| functions.function) {
        let frame = &functions
            .expect("a function unit is compiled with the program's functions")
            .frames[function.0 as usize];
        let (unit, unit_layout, unit_window) = crate::function_unit::prepare(sir_eu, layout, frame)
            .map_err(|message| ChainedEmitError::Emit(EmitError::Lowering(message)))?;
        function_layout = Some(unit_layout);
        window = Some(unit_window);
        prepared = Some(unit);
    }
    let layout = function_layout.as_ref().unwrap_or(layout);
    let sir_eu = prepared.as_ref().unwrap_or(sir_eu);
    let split = crate::function_unit::split_at_calls(sir_eu);
    let sir_eu = split.as_ref().unwrap_or(sir_eu);
    let frame = FunctionFrame {
        window,
        call_stack: layout.call_stack,
    };
    // Shared borrow so the checkpoint closure and downstream callees observe
    // the same predicate without moving it.
    let is_cancelled = &is_cancelled;
    // Observed between emission stages so a cancelled compile unwinds at the
    // next boundary instead of finishing the remaining phases. Callers
    // without cancellation pass `|| false`, which folds away after inlining.
    let checkpoint = || -> Result<(), ChainedEmitError> {
        if is_cancelled() {
            Err(ChainedEmitError::Cancelled)
        } else {
            Ok(())
        }
    };
    sir_eu.verify_result().map_err(PrepareError::Sir)?;
    let tick_loop = label == "eval_comb_apply_ff" && native_tick_loop;
    let check_runtime_events = tick_loop && crate::function_unit::may_raise_runtime_events(sir_eu);
    if let Some(trace) = trace.as_deref_mut() {
        trace.optimized_sir = sir_eu.to_string();
    }
    let semantic_size = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .ok_or(PrepareError::StateSizeOverflow)?;
    let mut function = crate::isel::lower_execution_unit_with_functions(
        sir_eu,
        layout,
        four_state,
        &crate::NativeDiagnostics::default(),
        functions,
    );
    crate::mir_opt::optimize(&mut function);
    crate::mir_legalize::legalize_variable_shift_counts(&mut function);
    checkpoint()?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.mir_before_regalloc = format!("{function:#?}");
    }
    let allocation =
        crate::regalloc::allocate_with_spills(function, is_cancelled).map_err(|error| {
            if matches!(error, crate::regalloc::TargetRegallocError::Cancelled) {
                ChainedEmitError::Cancelled
            } else {
                ChainedEmitError::Emit(EmitError::Lowering(error.to_string()))
            }
        })?;
    checkpoint()?;
    if let Some(trace) = trace {
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
    Ok(AllocatedFunction {
        allocation,
        semantic_size,
        tick_loop,
        check_runtime_events,
        frame,
    })
}

/// Emit the canonical no-op simulation kernel directly in AArch64 MIR.
pub fn emit_empty(state_size: usize) -> Result<EmitResult, EmitError> {
    let mut block = crate::mir::MBlock::new(BlockId(0));
    block.push(MInst::Return);
    let function = MFunction::new(vec![block], Vec::new());
    let allocation = crate::regalloc::allocate_with_spills(function, || false)
        .map_err(|error| EmitError::Lowering(error.to_string()))?;
    emit_function(
        &allocation.allocated.function,
        &allocation.allocated.assignment,
        allocation.spill_frame_size,
        state_size,
        &allocation.allocated.edge_copies,
        false,
        false,
        FunctionFrame::default(),
    )
}

/// Move SP down (`release` false) or back up by `bytes`, a multiple of 16
/// below 16 MiB. Register 31 of ADD/SUB (immediate) is SP.
fn emit_stack_adjust(ops: &mut VecAssembler<Aarch64Relocation>, bytes: u32, release: bool) {
    let sign = if release { 1 } else { -1 };
    for part in [i64::from(bytes & !0xfff), i64::from(bytes & 0xfff)] {
        if part != 0 {
            let emitted = emit_add_sub_immediate(ops, 31, 31, sign * part);
            debug_assert!(emitted, "the arena size is checked");
        }
    }
}

/// Check what emission relies on in a function unit: it runs no tick loop,
/// and only accesses whose base the emitter selects (and so redirects to the
/// frame register) reach its frame window.
fn verify_function_unit(
    function: &MFunction,
    window: crate::function_unit::FrameWindow,
    tick_loop: bool,
) -> Result<(), EmitError> {
    if tick_loop {
        return Err(EmitError::Unsupported("a tick loop in a function unit"));
    }
    let start = i64::from(window.base);
    let end = start + i64::from(window.size);
    let overlaps = |offset: i32, bytes: usize| {
        let offset = i64::from(offset);
        offset < end && offset + bytes as i64 > start
    };
    for instruction in function.blocks.iter().flat_map(|block| &block.insts) {
        let direct = match instruction {
            MInst::MemCopy {
                src_offset,
                dst_offset,
                byte_len,
            } => overlaps(*src_offset, *byte_len) || overlaps(*dst_offset, *byte_len),
            MInst::MemFill {
                dst_offset,
                byte_len,
                ..
            } => overlaps(*dst_offset, *byte_len),
            MInst::PackedLaneCompare { offset, .. } => overlaps(*offset, 1),
            _ => false,
        };
        if direct {
            return Err(EmitError::Unsupported(
                "a frame slot access the frame register cannot address",
            ));
        }
    }
    Ok(())
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
    frame: FunctionFrame,
) -> Result<EmitResult, EmitError> {
    if let Some(window) = frame.window {
        verify_function_unit(function, window, tick_loop)?;
    }
    let emit = |direct_branches| {
        emit_function_with_branches(
            function,
            assignment,
            spill_frame_size,
            state_size,
            plan,
            tick_loop,
            check_runtime_events,
            frame,
            direct_branches,
        )
    };
    // Conditional branches span +/- 1 MiB. Assemble compact branches first;
    // oversized kernels retain the nearby copy stubs and long jumps.
    match emit(true) {
        Err(EmitError::Assembly(DynasmError::ImpossibleRelocation(_))) => emit(false),
        result => result,
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_function_with_branches(
    function: &MFunction,
    assignment: &Assignment<VReg>,
    spill_frame_size: u32,
    state_size: usize,
    plan: &EdgeCopyPlan<BlockId>,
    tick_loop: bool,
    check_runtime_events: bool,
    frame: FunctionFrame,
    direct_branches: bool,
) -> Result<EmitResult, EmitError> {
    // A function unit may be running more than once at a time, so its arena
    // is on the machine stack, from offset 0 at SP, one per activation.
    let spill_base = if frame.window.is_some() {
        0
    } else {
        align16(state_size)?
    };
    let temporary_offset = spill_base
        .checked_add(spill_frame_size as usize)
        .and_then(|value| value.checked_add(15))
        .map(|value| value & !15)
        .ok_or(EmitError::Range("spill arena overflow"))?;
    let arena_end = temporary_offset
        .checked_add(TEMPORARY_BYTES)
        .ok_or(EmitError::Range("native arena size overflow"))?;
    let stack_arena = if frame.window.is_some() {
        u32::try_from(arena_end)
            .ok()
            .filter(|&bytes| bytes < 1 << 24)
            .ok_or(EmitError::Range("a function unit's arena exceeds 16 MiB"))?
    } else {
        0
    };
    let required_state_size = if frame.window.is_some() {
        state_size
    } else {
        arena_end
    };
    let required_state_size = u32::try_from(required_state_size)
        .map_err(|_| EmitError::Range("native arena exceeds u32"))?;

    let mut ops = VecAssembler::<Aarch64Relocation>::new(0);
    let forwarded = blocks::forwarded_blocks(function, plan);
    let emission_blocks = function
        .blocks
        .iter()
        .filter(|block| !forwarded.contains_key(&block.id))
        .collect::<Vec<_>>();
    let mut block_labels = emission_blocks
        .iter()
        .map(|block| (block.id, ops.new_dynamic_label()))
        .collect::<HashMap<_, _>>();
    for (&block, &target) in &forwarded {
        block_labels.insert(block, block_labels[&target]);
    }
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
    let mut state_pages = select_state_base_pages(function, secondary_page_registers);
    if frame.window.is_some() {
        // x29 holds the frame address instead.
        state_pages.primary = None;
        state_pages.frame = frame.window;
    }
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
    if tick_loop || state_pages.primary.is_some() || frame.window.is_some() {
        // x29 is reserved for the native-loop counter, the cached state-page
        // base or a function unit's frame, so preserve the caller's value
        // across the call.
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
    if frame.window.is_some() {
        // A function unit takes its frame's address after the state pointer.
        dynasm!(ops
            ; .arch aarch64
            ; mov X(FRAME_REG), x1
        );
        emit_stack_adjust(&mut ops, stack_arena, false);
        // ADD (immediate) reads register 31 as SP: x28 = SP.
        let moved = emit_add_sub_immediate(&mut ops, SPILL_REG, 31, 0);
        debug_assert!(moved);
    } else {
        emit_address_to(&mut ops, SPILL_REG, STATE_REG, spill_base as i64);
    }
    if let Some(page) = state_pages.primary {
        emit_address_to(&mut ops, STATE_PAGE_REG, STATE_REG, page);
    }
    for &(register, page) in state_pages.secondary.iter().flatten() {
        emit_address_to(&mut ops, register, STATE_REG, page);
    }
    let call_saves = extern_call::call_saves(function, assignment)?;
    // Every guard of a function call that fails traps here.
    let call_trap = ops.new_dynamic_label();
    let mut call_trap_used = false;
    // d29 holds the state pointer and d31 the tick count in a tick loop.
    let pinned_fp: &[u8] = match (tick_loop, tick_counter_in_fp) {
        (false, _) => &[],
        (true, false) => &[29],
        (true, true) => &[29, 31],
    };
    for (block_index, block) in emission_blocks.iter().enumerate() {
        let next_block = emission_blocks.get(block_index + 1).map(|block| block.id);
        let label = block_labels[&block.id];
        block_offsets.push((block.id, ops.offset().0 as u64));
        dynasm!(ops
            ; .arch aarch64
            ; =>label
        );
        for (index, instruction) in block.insts.iter().enumerate() {
            if let MInst::CallExtern { dst, func, args } = instruction {
                if let Some(frame_size) = celox_sir::extern_abi::native_function_guard(*func) {
                    extern_call::emit_call_guard(
                        &mut ops,
                        resolve(assignment, args[0])?,
                        frame_size,
                        frame
                            .call_stack
                            .ok_or(EmitError::Unsupported("a call without a call stack"))?,
                        call_trap,
                    );
                    call_trap_used = true;
                    continue;
                }
                extern_call::emit_call_extern(
                    &mut ops,
                    assignment,
                    *dst,
                    *func,
                    args,
                    &call_saves[&(block.id, index)],
                    pinned_fp,
                    frame.call_stack,
                )?;
                if celox_sir::extern_abi::native_function_slot(*func).is_some()
                    || celox_sir::extern_abi::native_kernel_index(*func).is_some()
                {
                    // A nonzero status of the callee, which the call left in
                    // x16, ends this function with it, as `ReturnError` does.
                    let returned = ops.new_dynamic_label();
                    dynasm!(ops ; .arch aarch64 ; cbz x16, =>returned);
                    if tick_loop {
                        if tick_counter_in_fp {
                            dynasm!(ops
                                ; .arch aarch64
                                ; fmov x17, d31
                                ; sub x17, x17, #1
                                ; fmov d31, x17
                            );
                        } else {
                            dynasm!(ops ; .arch aarch64 ; sub x29, x29, #1);
                        }
                    }
                    dynasm!(ops
                        ; .arch aarch64
                        ; mov x0, x16
                        ; b =>epilogue
                        ; =>returned
                    );
                }
                continue;
            }
            emit_instruction(
                &mut ops,
                instruction,
                block.id,
                next_block,
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
                direct_branches,
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
    if frame.window.is_some() {
        emit_stack_adjust(&mut ops, stack_arena, true);
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
    if call_trap_used {
        // A broken call-stack contract is a defect of the producer, not a
        // simulation error: stop as Cranelift's trap does.
        dynasm!(ops ; .arch aarch64 ; =>call_trap ; brk #1);
    }
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
