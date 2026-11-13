//! NativeBackend: SimBackend implementation using a custom host backend.
//!
//! Mirrors the structure of JitBackend but compiles through
//! ISel → scalar MIR → regalloc → host emission instead of Cranelift.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bit_set::BitSet;
use celox_design::{
    InitialStateData, InitialStateValue, InitialStateWriteRun, RuntimeCombObserver,
    RuntimeErrorInfo, RuntimeEventSite, RuntimeSchema,
};
use celox_runtime::DesignReflection;
use celox_runtime::backend::SimBackend;
use celox_testbench::TestbenchProgram;
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};

use crate::ir::{
    AbsoluteAddr, BlockId, ExecutionUnit, LaidOutProgram, RegionedAbsoluteAddr, RegisterId,
    SIRInstruction, SIROffset, SIRTerminator, SignalArrayLayout, SignalRef,
};
use crate::{CodegenError, HashMap, HashSet, SimulatorError, SimulatorOptions};

use super::super::RuntimeEventBuffer;
use super::super::compile_cancel::{CompileCancel, cancelled, cancelled_error};
use super::super::traits::SimulatorErrorCode;
use super::super::{MemoryLayout, get_byte_size, memory_image::MemoryImage};
use super::code_bytes::NativeCodeBytes;
#[cfg(any(
    feature = "x86_64-codegen",
    all(target_arch = "x86_64", not(feature = "arm64-codegen"))
))]
use super::regalloc;
use super::{emit, jit_mem};

mod comb_split;
mod compile_program;
mod compile_units;
#[cfg(hotspot_jit)]
mod hotspot;
mod program_image;
mod sim_backend;
mod trace;
use comb_split::*;
use compile_program::*;
pub(crate) use compile_units::*;
#[cfg(hotspot_jit)]
pub(crate) use hotspot::*;
pub use program_image::*;
use trace::*;

const NATIVE_FEATURE_BMI2: u8 = 1 << 0;
const NATIVE_FEATURE_AVX: u8 = 1 << 1;
const NATIVE_FEATURE_FS_STATE_BASE: u8 = 1 << 2;
const NATIVE_FEATURE_GS_STATE_BASE: u8 = 1 << 3;
const NATIVE_FEATURE_POPCNT: u8 = 1 << 4;
const NATIVE_FEATURE_BMI1: u8 = 1 << 5;
const KNOWN_NATIVE_FEATURES: u8 = NATIVE_FEATURE_BMI2
    | NATIVE_FEATURE_AVX
    | NATIVE_FEATURE_FS_STATE_BASE
    | NATIVE_FEATURE_GS_STATE_BASE
    | NATIVE_FEATURE_POPCNT
    | NATIVE_FEATURE_BMI1;

fn current_native_feature_bits() -> u8 {
    #[cfg(any(
        feature = "x86_64-codegen",
        all(target_arch = "x86_64", not(feature = "arm64-codegen"))
    ))]
    {
        celox_backend_x86::native::features::detected_image_feature_bits()
    }
    #[cfg(any(
        feature = "arm64-codegen",
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    {
        0
    }
}

fn format_native_feature_bits(bits: u8) -> String {
    let mut names = Vec::new();
    if bits & NATIVE_FEATURE_BMI1 != 0 {
        names.push("BMI1");
    }
    if bits & NATIVE_FEATURE_BMI2 != 0 {
        names.push("BMI2");
    }
    if bits & NATIVE_FEATURE_AVX != 0 {
        names.push("AVX");
    }
    if bits & NATIVE_FEATURE_POPCNT != 0 {
        names.push("POPCNT");
    }
    if bits & NATIVE_FEATURE_FS_STATE_BASE != 0 {
        names.push("FS state base");
    }
    if bits & NATIVE_FEATURE_GS_STATE_BASE != 0 {
        names.push("GS state base");
    }
    names.join(", ")
}

// ────────────────────────────────────────────────────────────────
// Event handle
// ────────────────────────────────────────────────────────────────

/// JIT function type: `fn(state: *mut u8) -> i64`
#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
pub type NativeSimFunc = unsafe extern "sysv64" fn(*mut u8) -> i64;
#[cfg(any(
    feature = "arm64-codegen",
    all(target_arch = "aarch64", not(feature = "x86_64-codegen")),
    all(feature = "x86_64-codegen", not(target_arch = "x86_64"))
))]
pub type NativeSimFunc = unsafe extern "C" fn(*mut u8) -> i64;

/// Time spent inside generated native simulator functions.
///
/// Timing is opt-in so normal simulation does not pay for host clock reads.
/// A call may execute many ticks when the native tick loop is enabled.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NativeExecutionTiming {
    elapsed: Duration,
    calls: u64,
}

impl NativeExecutionTiming {
    pub fn elapsed(self) -> Duration {
        self.elapsed
    }

    pub fn calls(self) -> u64 {
        self.calls
    }
}

/// Compiled event handle for native backend.
/// Holds the function pointer directly — no indirection at call time.
#[derive(Clone, Copy)]
pub struct NativeEventRef {
    pub func: NativeSimFunc,
    /// The event's combined comb-then-FF function, or `None` when the tick
    /// runs the shared comb function and then `func`.
    pub comb_apply_func: Option<NativeSimFunc>,
    pub addr: AbsoluteAddr,
    pub id: usize,
}

impl std::fmt::Debug for NativeEventRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeEventRef")
            .field("func", &(self.func as usize))
            .field(
                "comb_apply_func",
                &self.comb_apply_func.map(|func| func as usize),
            )
            .field("addr", &self.addr)
            .field("id", &self.id)
            .finish()
    }
}

impl super::super::EventHandle for NativeEventRef {
    fn id(&self) -> usize {
        self.id
    }
    fn addr(&self) -> AbsoluteAddr {
        self.addr
    }
}

// ────────────────────────────────────────────────────────────────
// Shared compiled code
// ────────────────────────────────────────────────────────────────

/// A warning or an error of a memory file the initial state of an instance
/// loads (see [`NativeBackend::load_initial_memory_files`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialMemoryFileReport {
    pub severity: celox_design::Severity,
    pub message: String,
    pub location: Option<celox_design::ReportLocation>,
}

/// Shared compiled code for the native backend.
/// Can be cloned (via Arc) to create multiple simulator instances
/// that share the same compiled machine code.
pub struct SharedNativeCode {
    comb_func: NativeSimFunc,
    clock_alias_funcs: Vec<Option<NativeSimFunc>>,
    comb_unit_funcs: Vec<NativeSimFunc>,
    /// One function per process kernel.
    process_funcs: Vec<NativeSimFunc>,
    /// Lane-partitioned kernels and their task functions.
    lane_kernels: Option<super::lanes::NativeLaneKernels>,
    /// Keep the combined executable image alive so every entry pointer remains
    /// valid. The image contains all native functions and their trailing
    /// constant/literal data.
    _jit_image: jit_mem::JitCode,
    program_image: NativeProgramImage,

    event_map: HashMap<AbsoluteAddr, NativeEventRef>,
    eval_only_event_map: HashMap<AbsoluteAddr, NativeEventRef>,
    apply_event_map: HashMap<AbsoluteAddr, NativeEventRef>,
    id_to_addr: Vec<AbsoluteAddr>,
    id_to_event: Vec<NativeEventRef>,
    layout: MemoryLayout,
    /// Simulation-state bytes plus the largest native spill/scratch arena
    /// required by any compiled function.
    pub(crate) native_memory_size: usize,
    options: NativeRuntimeOptions,
    /// (offset, byte_size) pairs for 4-state variables that need X initialization.
    four_state_inits: Vec<(usize, usize)>,
    /// The extern functions the code calls.
    extern_functions: crate::dpi::ExternFunctionTable,
}

// Safety: JitCode contains Mmap which is Send+Sync after creation.
unsafe impl Send for SharedNativeCode {}
unsafe impl Sync for SharedNativeCode {}

impl celox_runtime::backend::NativeProcessCode for SharedNativeCode {
    unsafe fn invoke(
        &self,
        index: usize,
        state: *mut u8,
        size: usize,
    ) -> Result<(), SimulatorErrorCode> {
        if size < self.layout.total_size {
            return Err(SimulatorErrorCode::InternalError);
        }
        let func = *self
            .process_funcs
            .get(index)
            .ok_or(SimulatorErrorCode::InternalError)?;
        // SAFETY: the invocation owner retains this code and its live image.
        unsafe { NativeBackend::call_raw(state, func) }
    }
}

impl SharedNativeCode {
    /// Attach a compiler-produced image to the precompiled host runtime.
    ///
    /// # Safety
    ///
    /// The image's machine code must come from a trusted source. Structural
    /// validation and the container checksum detect corruption, but do not
    /// authenticate code before it is mapped executable and invoked.
    pub unsafe fn from_image(program_image: NativeProgramImage) -> Result<Self, SimulatorError> {
        // Safety: upheld by this constructor's caller.
        unsafe { Self::from_image_with_dpi(program_image, &crate::DpiSymbols::default()) }
    }

    /// Attach a compiler-produced image, linking the extern functions its
    /// code calls from `dpi`.
    ///
    /// # Safety
    ///
    /// As for [`Self::from_image`].
    pub unsafe fn from_image_with_dpi(
        program_image: NativeProgramImage,
        dpi: &crate::DpiSymbols,
    ) -> Result<Self, SimulatorError> {
        program_image.validate().map_err(|message| {
            codegen_message(format!("invalid native program image: {message}"))
        })?;
        let unavailable = program_image.required_native_features & !current_native_feature_bits();
        if unavailable != 0 {
            return Err(codegen_message(format!(
                "native program image requires unavailable host features: {}",
                format_native_feature_bits(unavailable)
            )));
        }
        // Loading DPI-C libraries runs their initializers and keeps them
        // loaded, so it waits until the image is known to be usable.
        let mut extern_functions = dpi.resolve(&program_image.runtime_schema.extern_functions)?;
        let symbols = program_image
            .symbols
            .iter()
            .map(|symbol| jit_mem::JitSymbol {
                offset: symbol.offset,
                size: symbol.size,
                name: symbol.name.clone(),
            })
            .collect::<Vec<_>>();
        #[cfg(all(
            target_os = "linux",
            target_arch = "x86_64",
            not(feature = "arm64-codegen")
        ))]
        let mapped = match &program_image.code {
            NativeCodeBytes::Mapped { owner, range }
                if program_image
                    .code_image()
                    .as_ptr()
                    .addr()
                    .is_multiple_of(64) =>
            {
                // Retain the previous executable allocation as a fallback if
                // file execution is unavailable (for example a no-exec mount).
                if owner.make_executable(range.clone()).is_ok() {
                    // Safety: image validation and the caller establish trusted
                    // target code; the Arc retains its immutable executable
                    // file mapping for every entry pointer's lifetime.
                    Some(unsafe {
                        jit_mem::JitCode::from_executable_owner(
                            owner.clone(),
                            program_image.code_image().as_ptr(),
                            program_image.code_image().len(),
                            "celox_native_image",
                            &symbols,
                            program_image.options.perf_map,
                        )
                    })
                } else {
                    None
                }
            }
            _ => None,
        };
        #[cfg(not(all(
            target_os = "linux",
            target_arch = "x86_64",
            not(feature = "arm64-codegen")
        )))]
        let mapped: Option<Result<jit_mem::JitCode, std::io::Error>> = None;
        let jit_image = mapped
            .unwrap_or_else(|| {
                jit_mem::JitCode::new_named_with_symbols_profiled(
                    program_image.code_image(),
                    "celox_native_image",
                    &symbols,
                    program_image.options.perf_map,
                )
            })
            .map_err(|source| codegen_err(CodegenError::NativeMemory { source }))?;
        let kernel_addresses = program_image
            .runtime_schema
            .native_kernel_offsets
            .iter()
            .map(|&offset| native_function_at(&jit_image, offset).map(|func| func as usize))
            .collect::<Result<Vec<_>, _>>()?;
        extern_functions.append_native_kernels(kernel_addresses);
        let materialize = |event: NativeEventImageRef| -> Result<NativeEventRef, SimulatorError> {
            Ok(NativeEventRef {
                func: native_function_at(&jit_image, event.func_offset)?,
                comb_apply_func: event
                    .comb_apply_offset
                    .map(|offset| native_function_at(&jit_image, offset))
                    .transpose()?,
                addr: event.addr,
                id: event.id,
            })
        };
        let materialize_map = |source: &HashMap<AbsoluteAddr, NativeEventImageRef>| {
            source
                .iter()
                .map(|(&addr, &event)| Ok((addr, materialize(event)?)))
                .collect::<Result<HashMap<_, _>, SimulatorError>>()
        };
        let comb_func = native_function_at(&jit_image, program_image.comb_offset)?;
        let comb_unit_funcs = program_image
            .comb_unit_offsets
            .iter()
            .copied()
            .map(|offset| native_function_at(&jit_image, offset))
            .collect::<Result<Vec<_>, _>>()?;
        let process_funcs = program_image
            .process_offsets
            .iter()
            .copied()
            .map(|offset| native_function_at(&jit_image, offset))
            .collect::<Result<Vec<_>, _>>()?;
        let event_map = materialize_map(&program_image.event_map)?;
        let eval_only_event_map = materialize_map(&program_image.eval_only_event_map)?;
        let apply_event_map = materialize_map(&program_image.apply_event_map)?;
        let lane_kernels = program_image
            .lanes
            .as_ref()
            .map(|lanes| {
                super::lanes::NativeLaneKernels::materialize(
                    lanes,
                    &jit_image,
                    |event| program_image.event_map.get(event).map(|event| event.id),
                    program_image.id_to_event.len(),
                )
            })
            .transpose()?;
        let id_to_event = program_image
            .id_to_event
            .iter()
            .copied()
            .map(materialize)
            .collect::<Result<Vec<_>, _>>()?;

        // Optional named exports keep older images usable: absence requests a
        // full settle. Export names are part of the native runtime contract.
        let mut clock_alias_funcs = vec![None; id_to_event.len()];
        for entry in &program_image.code_entries {
            if let Some(id) = entry.name.strip_prefix(crate::clock_aliases::EXPORT_PREFIX) {
                let id = id
                    .strip_suffix(']')
                    .and_then(|id| id.parse::<usize>().ok())
                    .filter(|&id| id < clock_alias_funcs.len())
                    .ok_or_else(|| codegen_message("invalid clock alias export"))?;
                if clock_alias_funcs[id].is_some() {
                    return Err(codegen_message("duplicate clock alias export"));
                }
                clock_alias_funcs[id] = Some(native_function_at(&jit_image, entry.offset)?);
            }
        }
        Ok(Self {
            comb_func,
            clock_alias_funcs,
            comb_unit_funcs,
            process_funcs,
            lane_kernels,
            _jit_image: jit_image,
            event_map,
            eval_only_event_map,
            apply_event_map,
            id_to_addr: program_image.id_to_addr.clone(),
            id_to_event,
            layout: program_image.layout.clone(),
            native_memory_size: program_image.native_memory_size,
            options: program_image.options,
            four_state_inits: program_image.four_state_inits.clone(),
            program_image,
            extern_functions,
        })
    }

    /// Returns a reference to the memory layout.
    pub fn layout(&self) -> &MemoryLayout {
        &self.layout
    }

    /// Exact relocatable native image used by this compiled design.
    ///
    /// Entry addresses are intentionally not serialized: consumers copy this
    /// image and resolve [`Self::code_entries`] relative to the new base.
    pub fn code_image(&self) -> &[u8] {
        self.program_image.code_image()
    }

    /// Named function entries inside [`Self::code_image`].
    pub fn code_entries(&self) -> &[NativeCodeEntry] {
        self.program_image.code_entries()
    }

    /// Pointer-free compiler artifact from which this runtime image was loaded.
    pub fn program_image(&self) -> &NativeProgramImage {
        &self.program_image
    }

    pub(crate) fn supports_forces(&self) -> bool {
        self.options.native_force_support
    }
}

fn codegen_err(error: CodegenError) -> SimulatorError {
    error.into()
}

pub(super) fn codegen_message(message: impl Into<String>) -> SimulatorError {
    codegen_err(CodegenError::message(message))
}

// ────────────────────────────────────────────────────────────────
// NativeBackend
// ────────────────────────────────────────────────────────────────

pub struct NativeBackend {
    compiled: Arc<SharedNativeCode>,
    memory: MemoryImage,
    runtime_event_buffer: Arc<RuntimeEventBuffer>,
    comb_capture_enabled: Vec<u8>,
    /// Boxed so the address in the state header survives moves.
    heap: Box<celox_heap::Heap>,
    execution_timing: Option<NativeExecutionTiming>,
    /// Whether generated calls go through [`Self::call_func_slow`]: timing
    /// is on, or settles are cross-checked.
    slow_calls: bool,
    /// Worker threads for lane-partitioned kernels, started on first use.
    lane_pool: Option<celox_runtime::parallel::LanePool>,
    /// Per-kernel choice between partitioned and sequential code.
    lane_selectors: Box<super::super::lanes::LaneSelectors>,
}

fn write_bits_to_memory_from(
    memory: &mut [u8],
    destination_bit_offset: usize,
    bit_width: usize,
    source: &[u8],
    source_bit_offset: usize,
) {
    for bit in 0..bit_width {
        let source_bit = source_bit_offset + bit;
        let value = (source[source_bit / 8] >> (source_bit % 8)) & 1;
        let destination_bit = destination_bit_offset + bit;
        let destination = &mut memory[destination_bit / 8];
        let mask = 1u8 << (destination_bit % 8);
        if value == 0 {
            *destination &= !mask;
        } else {
            *destination |= mask;
        }
    }
}

fn write_bits_to_memory(
    memory: &mut [u8],
    destination_bit_offset: usize,
    bit_width: usize,
    source: &[u8],
) {
    write_bits_to_memory_from(memory, destination_bit_offset, bit_width, source, 0);
}

fn write_initial_run_to_plane(
    memory: &mut [u8],
    signal: SignalRef,
    mask_plane: bool,
    run: &InitialStateWriteRun,
    source: &[u8],
) {
    let Some(array) = signal.array_layout else {
        let plane_size = signal.width.div_ceil(8);
        let destination_bit_offset =
            (signal.offset + usize::from(mask_plane) * plane_size) * 8 + run.bit_offset;
        write_bits_to_memory(memory, destination_bit_offset, run.bit_width, source);
        return;
    };

    let plane_offset = signal.offset + usize::from(mask_plane) * array.plane_size;
    let mut consumed = 0usize;
    while consumed < run.bit_width {
        let logical_offset = run.bit_offset + consumed;
        let element = logical_offset / array.element_width;
        let intra_element = logical_offset % array.element_width;
        let part_width = (run.bit_width - consumed).min(array.element_width - intra_element);
        let destination_bit_offset =
            (plane_offset + element * array.element_stride) * 8 + intra_element;

        if consumed.is_multiple_of(8)
            && destination_bit_offset.is_multiple_of(8)
            && part_width.is_multiple_of(8)
        {
            let source_byte = consumed / 8;
            let destination_byte = destination_bit_offset / 8;
            let byte_width = part_width / 8;
            memory[destination_byte..destination_byte + byte_width]
                .copy_from_slice(&source[source_byte..source_byte + byte_width]);
        } else {
            write_bits_to_memory_from(memory, destination_bit_offset, part_width, source, consumed);
        }
        consumed += part_width;
    }
}

impl NativeBackend {
    /// Whether `signal` is a scalar of exactly `T`'s bytes.
    #[inline]
    fn exact_scalar<T>(signal: SignalRef) -> bool {
        signal.width == std::mem::size_of::<T>() * 8 && signal.array_layout.is_none()
    }

    /// Write `val` to the scalar or array `signal` without marking it for
    /// waveforms or settle partitions.
    fn store_scalar<T: Copy>(&mut self, signal: SignalRef, val: T) {
        let allocated_size = get_byte_size(signal.width);
        let provided_size = std::mem::size_of::<T>();
        let clear_mask = self.compiled.options.four_state && signal.is_4state;

        let copy_size = provided_size.min(allocated_size);

        if signal.array_layout.is_some() {
            let value_bytes =
                unsafe { std::slice::from_raw_parts(&val as *const T as *const u8, copy_size) };
            self.write_signal_plane(signal, false, &BigUint::from_bytes_le(value_bytes));
            if clear_mask {
                self.write_signal_plane(signal, true, &BigUint::from(0u8));
            }
            return;
        }

        unsafe {
            let base_ptr = (self.memory.as_mut_ptr() as *mut u8).add(signal.offset);
            if !clear_mask && allocated_size == 1 {
                let raw = if copy_size == 0 {
                    0
                } else {
                    *(&val as *const T as *const u8)
                };
                let byte = if signal.width < 8 {
                    raw & ((1u8 << signal.width) - 1)
                } else {
                    raw
                };
                *base_ptr = byte;
                return;
            }

            if copy_size < allocated_size {
                std::ptr::write_bytes(base_ptr, 0, allocated_size);
            }
            std::ptr::copy_nonoverlapping(&val as *const T as *const u8, base_ptr, copy_size);

            if !signal.width.is_multiple_of(8) && allocated_size != 0 {
                *base_ptr.add(allocated_size - 1) &= (1u8 << (signal.width % 8)) - 1;
            }

            if clear_mask {
                let mask_ptr = base_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0, allocated_size);
            }
        }
    }

    #[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
    pub(crate) fn compile_outlined_image_for_test(
        program: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<NativeProgramImage, SimulatorError> {
        compile_program_with_outlining_threshold(program, options, false, None, 0)
            .map(|(image, _)| image)
    }

    #[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
    pub(crate) fn split_comb_units_for_test(program: &mut LaidOutProgram) {
        program.sir.eval_comb = program
            .sir
            .eval_comb
            .iter()
            .flat_map(split_comb_execution_unit)
            .collect();
    }

    pub(crate) fn eval_comb_units_with(
        &mut self,
        mut after_unit: impl FnMut(&mut Self),
    ) -> Result<(), SimulatorErrorCode> {
        let funcs = self.compiled.comb_unit_funcs.clone();
        if funcs.is_empty() {
            return self.eval_comb();
        }
        for func in funcs {
            self.call_func_timed(func)?;
            after_unit(self);
        }
        Ok(())
    }

    /// Compile a pointer-free native image without attaching it to executable
    /// memory. A precompiled runtime can load the result with
    /// [`SharedNativeCode::from_image`].
    pub fn compile_image(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<NativeProgramImage, SimulatorError> {
        let (image, trace) = compile_program(laid_out, options, false, None)?;
        debug_assert!(trace.is_none());
        Ok(image)
    }

    /// Compile with cooperative cancellation observed at task and phase
    /// boundaries.
    ///
    /// When `cancel` is triggered the pipeline unwinds with
    /// [`CodegenError::Cancelled`] as soon as the unit in flight completes;
    /// an uncancelled run is unchanged apart from one flag load per
    /// boundary.
    pub(crate) fn compile_image_with_cancel(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
        cancel: &CompileCancel,
    ) -> Result<NativeProgramImage, SimulatorError> {
        let (image, trace) = compile_program(laid_out, options, false, Some(cancel))?;
        debug_assert!(trace.is_none());
        Ok(image)
    }

    /// Compile with cooperative cancellation plus code-generation tracing.
    pub(crate) fn compile_image_with_cancel_and_trace(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
        cancel: &CompileCancel,
    ) -> Result<(NativeProgramImage, NativeCodegenTrace), SimulatorError> {
        let (image, trace) = compile_program(laid_out, options, true, Some(cancel))?;
        Ok((
            image,
            trace.expect("trace-enabled native compilation must return a trace"),
        ))
    }

    pub(crate) fn compile_image_with_codegen_trace(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<(NativeProgramImage, NativeCodegenTrace), SimulatorError> {
        let (image, trace) = compile_program(laid_out, options, true, None)?;
        Ok((
            image,
            trace.expect("trace-enabled native compilation must return a trace"),
        ))
    }

    /// Load a compiler-produced image into executable memory and create a
    /// backend instance for it.
    ///
    /// # Safety
    ///
    /// The image's machine code must come from a trusted compiler or image
    /// container. Structural validation does not authenticate code before it
    /// is mapped executable and invoked.
    pub unsafe fn from_image(image: NativeProgramImage) -> Result<Self, SimulatorError> {
        // Safety: upheld by this constructor's caller.
        unsafe { Self::from_image_with_dpi(image, &crate::DpiSymbols::default()) }
    }

    /// Load a compiler-produced image, linking the extern functions its code
    /// calls from `dpi`.
    ///
    /// # Safety
    ///
    /// As for [`Self::from_image`].
    pub unsafe fn from_image_with_dpi(
        image: NativeProgramImage,
        dpi: &crate::DpiSymbols,
    ) -> Result<Self, SimulatorError> {
        // Safety: upheld by this constructor's caller.
        let shared = Arc::new(unsafe { SharedNativeCode::from_image_with_dpi(image, dpi)? });
        Ok(Self::from_shared(shared))
    }

    pub fn new(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<Self, SimulatorError> {
        let image = Self::compile_image(laid_out, options)?;
        // Safety: `image` was produced in-process by the Celox compiler above.
        unsafe { Self::from_image_with_dpi(image, &options.dpi) }
    }

    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    pub(crate) fn new_with_codegen_trace(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<(Self, NativeCodegenTrace), SimulatorError> {
        let (image, trace) = Self::compile_image_with_codegen_trace(laid_out, options)?;
        // Safety: `image` was produced in-process by the Celox compiler above.
        let shared = unsafe { SharedNativeCode::from_image_with_dpi(image, &options.dpi)? };
        let backend = Self::from_shared(Arc::new(shared));
        Ok((backend, trace))
    }

    /// Create a new backend instance from shared compiled code.
    /// Each instance gets its own simulation state memory.
    pub fn from_shared(shared: Arc<SharedNativeCode>) -> Self {
        let mem_size_words = shared.native_memory_size.div_ceil(8);
        let mut memory = MemoryImage::zeroed(mem_size_words + 1); // +1 for safety
        let runtime_event_buffer = Arc::new(RuntimeEventBuffer::new(
            shared.layout.runtime_event_buffer_size,
        ));
        // Generated scheduler code clears these flags one complete word at a time.
        let comb_capture_enabled = vec![
            0;
            shared
                .layout
                .runtime_event_site_layouts
                .len()
                .max(1)
                .next_multiple_of(8)
        ];

        // Initialize 4-state regions to X (v=1, m=1)
        for &(offset, allocated_size) in &shared.four_state_inits {
            unsafe {
                let base_ptr = (memory.as_mut_ptr() as *mut u8).add(offset);
                std::ptr::write_bytes(base_ptr, 0xFF, allocated_size);
                let mask_ptr = base_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0xFF, allocated_size);
            }
        }

        let slow_calls = shared.layout.settle_check();
        let mut backend = Self {
            compiled: shared,
            memory,
            runtime_event_buffer,
            comb_capture_enabled,
            heap: Default::default(),
            execution_timing: None,
            slow_calls,
            lane_pool: None,
            lane_selectors: Default::default(),
        };
        backend.install_event_buffers();
        let compiled = Arc::clone(&backend.compiled);
        backend.apply_initial_values(&compiled.program_image.design.initial_state);
        backend
    }

    /// Build a backend from compiled code plus an existing live simulation
    /// state, e.g. when a tiered simulation promotes from the interpreter.
    ///
    /// The caller must guarantee the state was produced against the same
    /// laid-out program (identical layout), and that the event buffer `Arc`
    /// is the one referenced by the state header so its pointer stays valid.
    pub(crate) fn adopt_shared_with_state(
        shared: Arc<SharedNativeCode>,
        memory: MemoryImage,
        runtime_event_buffer: Arc<RuntimeEventBuffer>,
        mut comb_capture_enabled: Vec<u8>,
        heap: Box<celox_heap::Heap>,
    ) -> Self {
        comb_capture_enabled.resize(
            shared
                .layout
                .runtime_event_site_layouts
                .len()
                .max(1)
                .next_multiple_of(8),
            0,
        );
        let slow_calls = shared.layout.settle_check();
        let mut backend = Self {
            compiled: shared,
            memory,
            runtime_event_buffer,
            comb_capture_enabled,
            heap,
            execution_timing: None,
            slow_calls,
            lane_pool: None,
            lane_selectors: Default::default(),
        };
        // The state names the previous tier's extern function table; point it
        // at the one this code owns.
        backend.install_extern_functions();
        backend.install_heap();
        backend
    }

    fn apply_initial_values(&mut self, initial_state: &[InitialStateValue<AbsoluteAddr>]) {
        for init in initial_state {
            self.apply_initial_value(init);
        }
    }

    /// Read memory files from the host file system (IEEE 1800-2023 21.4),
    /// for an instance that runs without a [`crate::Simulator`], which reads
    /// them through its own file system: those its code loads when the
    /// loads execute, and those the initial state loads now, applying the
    /// initial state again, in order, with their words. Returns the
    /// warnings and errors of the initial loads.
    pub fn load_initial_memory_files(&mut self) -> Vec<InitialMemoryFileReport> {
        // So do the loads its code makes when they execute.
        let compiled = Arc::clone(&self.compiled);
        self.heap.set_memory_file_reader(
            crate::HostMemoryFileReader::new(compiled.program_image.design.memory_file_bases())
                .into_heap_reader(),
        );
        let initial_state = &compiled.program_image.design.initial_state;
        if !initial_state
            .iter()
            .any(|init| matches!(init.data, InitialStateData::MemoryFile(_)))
        {
            return Vec::new();
        }
        let mut reports = Vec::new();
        for init in initial_state {
            let InitialStateData::MemoryFile(load) = &init.data else {
                self.apply_initial_value(init);
                continue;
            };
            let outcome = celox_frontend_core::memory_file::load_memory_file(load, |path| {
                std::fs::read_to_string(path)
            });
            let signal = self.resolve_signal(&init.address);
            self.apply_initial_memory_writes(signal, &outcome.runs);
            for (severity, message) in [
                (celox_design::Severity::Warning, outcome.warning),
                (celox_design::Severity::Error, outcome.error),
            ] {
                if let Some(message) = message {
                    reports.push(InitialMemoryFileReport {
                        severity,
                        message,
                        location: load.location.clone(),
                    });
                }
            }
        }
        reports
    }

    fn apply_initial_value(&mut self, init: &InitialStateValue<AbsoluteAddr>) {
        {
            let signal = self.resolve_signal(&init.address);
            match &init.data {
                InitialStateData::Packed {
                    value,
                    mask,
                    written_mask,
                } if signal.array_layout.is_some() => {
                    // An unpacked array is written element by element.
                    let runs = InitialStateData::packed_write_runs(
                        value,
                        mask,
                        written_mask,
                        signal.width,
                        self.compiled.options.four_state && signal.is_4state,
                    );
                    self.apply_initial_memory_writes(signal, &runs);
                }
                InitialStateData::Packed {
                    value,
                    mask,
                    written_mask,
                } => {
                    let width_mask = if signal.width == 0 {
                        BigUint::default()
                    } else {
                        (BigUint::from(1u8) << signal.width) - BigUint::from(1u8)
                    };
                    let preserve_mask = &width_mask ^ (written_mask & &width_mask);
                    let (current_value, current_mask) = self.get_four_state(signal);
                    let value = (current_value & &preserve_mask) | (value & written_mask);
                    let mask = (current_mask & &preserve_mask) | (mask & written_mask);
                    if self.compiled.options.four_state && signal.is_4state {
                        self.set_four_state(signal, value, mask);
                    } else {
                        let known_mask = &width_mask ^ (&mask & &width_mask);
                        self.set_wide(signal, value & known_mask);
                    }
                }
                InitialStateData::Writes(runs) => self.apply_initial_memory_writes(signal, runs),
                // The simulator that runs the instance loads the file through
                // its file system, applying the initial state again.
                InitialStateData::MemoryFile(_) => {}
            }
        }
    }

    fn apply_initial_memory_writes(&mut self, signal: SignalRef, runs: &[InitialStateWriteRun]) {
        let value_byte_size = signal.width.div_ceil(8);
        let write_mask = self.compiled.options.four_state && signal.is_4state;
        let mem = self.mem_bytes_mut();

        for run in runs {
            if run.bit_width == 0 {
                continue;
            }
            // Two-state storage has no mask plane to hold an X or a Z.
            let run = if write_mask {
                std::borrow::Cow::Borrowed(run)
            } else {
                run.two_state()
            };
            let run = &*run;
            if signal.array_layout.is_some() {
                write_initial_run_to_plane(mem, signal, false, run, &run.value_bytes);
                if write_mask {
                    write_initial_run_to_plane(mem, signal, true, run, &run.mask_bytes);
                }
                continue;
            }
            if run.bit_offset.is_multiple_of(8) && run.bit_width.is_multiple_of(8) {
                let byte_offset = run.bit_offset / 8;
                let byte_width = run.bit_width / 8;
                let value_offset = signal.offset + byte_offset;
                mem[value_offset..value_offset + byte_width]
                    .copy_from_slice(&run.value_bytes[..byte_width]);
                if write_mask {
                    let mask_offset = signal.offset + value_byte_size + byte_offset;
                    mem[mask_offset..mask_offset + byte_width]
                        .copy_from_slice(&run.mask_bytes[..byte_width]);
                }
                continue;
            }

            write_bits_to_memory(
                mem,
                signal.offset * 8 + run.bit_offset,
                run.bit_width,
                &run.value_bytes,
            );
            if write_mask {
                write_bits_to_memory(
                    mem,
                    (signal.offset + value_byte_size) * 8 + run.bit_offset,
                    run.bit_width,
                    &run.mask_bytes,
                );
            }
        }
    }

    /// Start a fresh opt-in measurement of generated native function calls.
    pub fn start_execution_timing(&mut self) {
        self.execution_timing = Some(NativeExecutionTiming::default());
        self.slow_calls = true;
    }

    /// Total capacity of the live image in `u64` words.
    pub(crate) fn memory_word_capacity(&self) -> usize {
        self.memory.capacity_words()
    }

    /// Swap the compiled code image for a newly compiled one in place,
    /// keeping the live state, event buffers, capture flags, and timing.
    /// The caller must supply code from the same laid-out program and only
    /// replace it outside a split evaluate/apply pair: backend scratch may
    /// differ, but semantic state and trigger IDs must remain identical.
    /// The image capacity must cover the new `native_memory_size` requirement.
    pub(crate) fn replace_shared_code(&mut self, shared: Arc<SharedNativeCode>) {
        let mem_size_words = shared.native_memory_size.div_ceil(8) + 1;
        if self.memory.len_words() < mem_size_words {
            self.memory.resize_zeroed_within_capacity(mem_size_words);
        }
        self.compiled = shared;
        // The helper and image-local kernel table belongs to this code image.
        // Replacement may release the previous table, so rebind before any
        // generated function can execute against the transferred live state.
        self.install_extern_functions();
        self.slow_calls = self.execution_timing.is_some() || self.compiled.layout.settle_check();
    }

    /// Stop timing and return the accumulated generated-code interval.
    pub fn finish_execution_timing(&mut self) -> Option<NativeExecutionTiming> {
        self.slow_calls = self.compiled.layout.settle_check();
        self.execution_timing.take()
    }

    fn install_event_buffers(&mut self) {
        use crate::backend::memory_layout::{
            STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET, STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
        };

        self.memory.write_header_word(
            STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
            self.runtime_event_buffer.as_mut_ptr() as u64,
        );
        self.memory.write_header_word(
            STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET,
            self.comb_capture_enabled.as_ptr() as u64,
        );
        self.install_extern_functions();
        self.install_heap();
    }

    fn install_heap(&mut self) {
        self.memory.write_header_word(
            celox_state_layout::STATE_HEADER_HEAP_ADDR_OFFSET,
            std::ptr::from_mut(&mut *self.heap) as u64,
        );
    }

    fn install_extern_functions(&mut self) {
        self.memory.write_header_word(
            celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET,
            self.compiled.extern_functions.as_ptr() as u64,
        );
    }

    /// Get the shared compiled code handle.
    pub fn shared_code(&self) -> Arc<SharedNativeCode> {
        Arc::clone(&self.compiled)
    }

    fn mem_ptr(&self) -> *const u8 {
        self.memory.as_ptr() as *const u8
    }

    fn mem_mut_ptr(&mut self) -> *mut u8 {
        self.memory.as_mut_ptr() as *mut u8
    }

    fn mem_bytes(&self) -> &[u8] {
        let ptr = self.mem_ptr();
        let len = self.memory.len_words() * 8;
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }

    fn mem_bytes_mut(&mut self) -> &mut [u8] {
        let ptr = self.mem_mut_ptr();
        let len = self.memory.len_words() * 8;
        unsafe { std::slice::from_raw_parts_mut(ptr, len) }
    }

    fn read_signal_plane(&self, signal: SignalRef, mask_plane: bool) -> BigUint {
        let bytes = self.mem_bytes();
        let Some(array) = signal.array_layout else {
            let byte_size = get_byte_size(signal.width);
            let plane_offset = signal.offset + usize::from(mask_plane) * byte_size;
            let mut value = BigUint::from_bytes_le(&bytes[plane_offset..plane_offset + byte_size]);
            if !signal.width.is_multiple_of(8) {
                value &= (BigUint::from(1u8) << signal.width) - BigUint::from(1u8);
            }
            return value;
        };

        let plane_offset = signal.offset + usize::from(mask_plane) * array.plane_size;
        array.gather(&bytes[plane_offset..plane_offset + array.element_extent()])
    }

    fn write_signal_plane(&mut self, signal: SignalRef, mask_plane: bool, value: &BigUint) {
        let Some(array) = signal.array_layout else {
            let byte_size = get_byte_size(signal.width);
            let plane_offset = signal.offset + usize::from(mask_plane) * byte_size;
            let bytes = self.mem_bytes_mut();
            bytes[plane_offset..plane_offset + byte_size].fill(0);
            let value_bytes = value.to_bytes_le();
            let copy_len = value_bytes.len().min(byte_size);
            bytes[plane_offset..plane_offset + copy_len].copy_from_slice(&value_bytes[..copy_len]);
            if !signal.width.is_multiple_of(8) && byte_size != 0 {
                bytes[plane_offset + byte_size - 1] &= (1u8 << (signal.width % 8)) - 1;
            }
            return;
        };

        let plane_offset = signal.offset + usize::from(mask_plane) * array.plane_size;
        // A handle on part of an array keeps the whole array's plane size,
        // which locates the mask plane: only its own elements are written.
        let plane = &mut self.mem_bytes_mut()[plane_offset..plane_offset + array.element_extent()];
        plane.fill(0);
        array.scatter(plane, value);
    }

    /// Run one lane-partitioned kernel on the lane workers.
    ///
    /// The calling thread executes lane 0. If worker threads cannot be
    /// started, the kernel runs sequentially in its listed task order.
    fn run_lane_kernel(
        &mut self,
        lanes: u32,
        kernel: &super::lanes::NativeLaneKernel,
    ) -> Result<(), SimulatorErrorCode> {
        if self.lane_pool.is_none() {
            match celox_runtime::parallel::LanePool::new(lanes) {
                Ok(pool) => self.lane_pool = Some(pool),
                Err(error) => {
                    tracing::warn!("lane workers unavailable, running sequentially: {error}");
                }
            }
        }
        let start = self.execution_timing.is_some().then(Instant::now);
        let runner = super::lanes::NativeTaskRunner {
            memory: self.memory.as_mut_slice().as_mut_ptr() as *mut u8,
            functions: &kernel.functions,
        };
        let result = match &mut self.lane_pool {
            Some(pool) => pool.run(&kernel.schedule, &runner),
            None => kernel.schedule.run_sequential(&runner),
        };
        if let (Some(start), Some(timing)) = (start, self.execution_timing.as_mut()) {
            timing.elapsed = timing.elapsed.saturating_add(start.elapsed());
            timing.calls = timing.calls.saturating_add(1);
        }
        result.map_err(|failure| match failure.code {
            code if code > 0 || celox_runtime::comb_fatal_site(code).is_some() => {
                SimulatorErrorCode::DetectedTrueLoopCode(code)
            }
            _ => SimulatorErrorCode::InternalError,
        })
    }

    fn lane_comb_kernel(&self) -> bool {
        self.compiled
            .lane_kernels
            .as_ref()
            .is_some_and(|kernels| kernels.comb.is_some())
    }

    fn lane_fused_kernel(&self, event: NativeEventRef) -> bool {
        self.compiled.lane_kernels.as_ref().is_some_and(|kernels| {
            kernels
                .fused
                .get(event.id)
                .is_some_and(|kernel| kernel.is_some())
        })
    }

    fn lane_event_kernel(&self, event: NativeEventRef) -> bool {
        self.compiled.lane_kernels.as_ref().is_some_and(|kernels| {
            kernels
                .events
                .get(event.id)
                .is_some_and(|kernel| kernel.is_some())
        })
    }

    fn call_func(memory: &mut [u64], func: NativeSimFunc) -> Result<(), SimulatorErrorCode> {
        // SAFETY: this call exclusively owns the initialized image.
        unsafe { Self::call_raw(memory.as_mut_ptr().cast(), func) }
    }

    unsafe fn call_raw(ptr: *mut u8, func: NativeSimFunc) -> Result<(), SimulatorErrorCode> {
        // SAFETY: the caller supplies the native image and exclusive execution.
        let ret = unsafe { func(ptr) };
        match ret {
            0 => Ok(()),
            code if code > 0 || celox_runtime::comb_fatal_site(code).is_some() => {
                Err(SimulatorErrorCode::DetectedTrueLoopCode(code))
            }
            _ => Err(SimulatorErrorCode::InternalError),
        }
    }

    fn call_func_timed(&mut self, func: NativeSimFunc) -> Result<(), SimulatorErrorCode> {
        if !self.slow_calls {
            return Self::call_func(self.memory.as_mut_slice(), func);
        }
        self.call_func_slow(func)
    }

    #[cold]
    fn call_func_slow(&mut self, func: NativeSimFunc) -> Result<(), SimulatorErrorCode> {
        if func as usize == self.compiled.comb_func as usize && self.compiled.layout.settle_check()
        {
            let compiled = Arc::clone(&self.compiled);
            return crate::settle::run_checked(
                self,
                compiled.layout.settle.as_ref().expect("checked settle"),
                compiled.layout.total_size,
                |backend| backend.memory.as_mut_bytes(),
                |backend| backend.call_func_measured(func),
            );
        }
        self.call_func_measured(func)
    }

    fn call_func_measured(&mut self, func: NativeSimFunc) -> Result<(), SimulatorErrorCode> {
        let Some(_) = self.execution_timing else {
            return Self::call_func(self.memory.as_mut_slice(), func);
        };
        let start = Instant::now();
        let result = Self::call_func(self.memory.as_mut_slice(), func);
        let elapsed = start.elapsed();
        let timing = self
            .execution_timing
            .as_mut()
            .expect("native execution timing was enabled before the call");
        timing.elapsed = timing.elapsed.saturating_add(elapsed);
        timing.calls = timing.calls.saturating_add(1);
        result
    }

    fn call_func_many(
        memory: &mut [u64],
        func: NativeSimFunc,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        use crate::backend::memory_layout::STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET;

        if count == 0 {
            return (0, Ok(()));
        }
        let remaining_word = STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET / 8;
        memory[remaining_word] = count;
        let ptr = memory.as_mut_ptr() as *mut u8;
        let ret = unsafe { func(ptr) };
        let completed = count.saturating_sub(memory[remaining_word]);
        let result = match ret {
            0 => Ok(()),
            code if code > 0 || celox_runtime::comb_fatal_site(code).is_some() => {
                Err(SimulatorErrorCode::DetectedTrueLoopCode(code))
            }
            _ => Err(SimulatorErrorCode::InternalError),
        };
        (completed, result)
    }

    fn call_func_many_timed(
        &mut self,
        func: NativeSimFunc,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        if self.execution_timing.is_none() || count == 0 {
            return Self::call_func_many(self.memory.as_mut_slice(), func, count);
        }
        let start = Instant::now();
        let result = Self::call_func_many(self.memory.as_mut_slice(), func, count);
        let elapsed = start.elapsed();
        let timing = self
            .execution_timing
            .as_mut()
            .expect("native execution timing was enabled before the call");
        timing.elapsed = timing.elapsed.saturating_add(elapsed);
        timing.calls = timing.calls.saturating_add(1);
        result
    }
}

#[cfg(test)]
mod tests;
