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
#[cfg(any(
    feature = "x86_64-codegen",
    all(target_arch = "x86_64", not(feature = "arm64-codegen"))
))]
use super::regalloc;
use super::{emit, jit_mem};

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

/// Shared compiled code for the native backend.
/// Can be cloned (via Arc) to create multiple simulator instances
/// that share the same compiled machine code.
pub struct SharedNativeCode {
    comb_func: NativeSimFunc,
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
        let jit_image = jit_mem::JitCode::new_named_with_symbols_profiled(
            program_image.code_image(),
            "celox_native_image",
            &symbols,
            program_image.options.perf_map,
        )
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

        Ok(Self {
            comb_func,
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

/// One callable function in a packed native code image.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCodeEntry {
    /// Stable diagnostic name for the emitted function.
    pub name: String,
    /// Byte offset from the start of the native image.
    pub offset: usize,
    /// Size of this function blob, including its private literal data.
    pub size: usize,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct NativeEventImageRef {
    func_offset: usize,
    comb_apply_offset: Option<usize>,
    addr: AbsoluteAddr,
    id: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct NativeCodeSymbol {
    offset: usize,
    size: usize,
    name: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct NativeRuntimeOptions {
    four_state: bool,
    native_tick_loop: bool,
    native_force_support: bool,
    perf_map: bool,
    /// How lane-partitioned kernels are chosen over sequential ones.
    kernel_selection: celox_runtime::parallel::KernelSelection,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct NativeForceBinding {
    pub signal: celox_runtime::ReflectionSignalId,
    pub enabled: SignalRef,
    pub value: SignalRef,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NativeRuntimeSchema {
    pub(super) force_bindings: Vec<NativeForceBinding>,
    pub(crate) runtime_errors: HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
    pub(crate) runtime_event_sites: Vec<RuntimeEventSite>,
    /// Extern functions the code calls, resolved when the image is attached.
    pub(crate) extern_functions: Vec<celox_design::ExternFunction>,
    /// Image-local compiled callees appended to the DPI address table.
    pub(crate) native_kernel_offsets: Vec<usize>,
    pub(crate) comb_observers: Vec<RuntimeCombObserver<AbsoluteAddr>>,
    pub(crate) testbench_read_roots: HashSet<AbsoluteAddr>,
    pub(crate) rtl_accesses: std::collections::BTreeSet<AbsoluteAddr>,
    pub(crate) persistent_comb_state: std::collections::BTreeSet<AbsoluteAddr>,
    pub(crate) rtl_writes: HashSet<celox_design::VarAtomBase<AbsoluteAddr>>,
    /// Ordered so that the encoded image does not depend on hash order.
    pub(crate) comb_writes: std::collections::BTreeSet<AbsoluteAddr>,
    pub(crate) dead_store_policy: celox_design::DeadStorePolicy,
    /// Ordered so that the encoded image does not depend on hash order.
    pub(crate) live_signals: std::collections::BTreeSet<AbsoluteAddr>,
    pub(crate) declaration_startup: Option<celox_design::DeclarationStartup<AbsoluteAddr>>,
    pub(crate) processes: Vec<celox_design::ProcessSlots<AbsoluteAddr>>,
    pub(crate) process_origins: Vec<celox_design::ProcessOrigin>,
    pub(crate) process_frames: Vec<Option<celox_design::ProcessFrame<AbsoluteAddr>>>,
    pub(crate) watches: Vec<celox_design::EventWatch<AbsoluteAddr>>,
    pub(crate) occurrence_probes: Vec<(celox_design::VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>,
    pub(crate) wait_sites: Vec<celox_design::WaitSite<AbsoluteAddr>>,
    pub(crate) nba_sites: Vec<celox_design::NbaSite<AbsoluteAddr>>,
    pub(crate) host_request_sites: Vec<celox_design::HostRequestSite<AbsoluteAddr>>,
    pub(crate) active_commits: Vec<AbsoluteAddr>,
    pub(crate) time_slots: Vec<AbsoluteAddr>,
    #[serde(default)]
    pub(crate) sample_shadows: Vec<(AbsoluteAddr, AbsoluteAddr)>,
    #[serde(default)]
    pub(crate) initial_copies: Vec<(AbsoluteAddr, AbsoluteAddr)>,
    pub(crate) scheduler: Option<celox_design::SchedulerSchema<AbsoluteAddr>>,
    pub(crate) derived_edges: Vec<celox_design::DerivedEdgeUnits<AbsoluteAddr>>,
}

/// Pointer-free native compiler artifact which can be attached to the
/// precompiled Celox runtime.
#[derive(Clone, Serialize, Deserialize)]
pub struct NativeProgramImage {
    code: Vec<u8>,
    code_entries: Vec<NativeCodeEntry>,
    symbols: Vec<NativeCodeSymbol>,
    comb_offset: usize,
    comb_unit_offsets: Vec<usize>,
    /// Entry offset of each process kernel.
    process_offsets: Vec<usize>,
    required_native_features: u8,
    event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    eval_only_event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    apply_event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    id_to_addr: Vec<AbsoluteAddr>,
    id_to_event: Vec<NativeEventImageRef>,
    reflection: DesignReflection,
    design: crate::ir::RuntimeDesign,
    testbench: Option<TestbenchProgram<AbsoluteAddr>>,
    runtime_schema: NativeRuntimeSchema,
    layout: MemoryLayout,
    native_memory_size: usize,
    options: NativeRuntimeOptions,
    four_state_inits: Vec<(usize, usize)>,
    /// Lane-partitioned kernels, one function per task.
    lanes: Option<super::lanes::NativeLaneImage>,
}

impl NativeProgramImage {
    /// Physical duration of one tick, retained in serialized native images.
    pub fn time_precision(&self) -> celox_design::TimePrecision {
        self.design.time_precision
    }

    /// Remap filesystem paths in the image's runtime and diagnostic metadata.
    /// This permits relative paths in stored artifacts and rebinding on load.
    /// Machine code and design-authored literal strings are not modified.
    /// A failed mapping can leave some paths remapped; discard that image.
    pub fn try_map_paths<E>(
        &mut self,
        mut map: impl FnMut(&std::path::Path) -> Result<std::path::PathBuf, E>,
    ) -> Result<(), E> {
        if let Some(testbench) = &mut self.testbench {
            testbench.try_map_paths(&mut map)?;
        }
        Ok(())
    }

    /// Complete relocatable machine-code image.
    pub fn code_image(&self) -> &[u8] {
        &self.code
    }

    /// Named entry offsets in [`Self::code_image`].
    pub fn code_entries(&self) -> &[NativeCodeEntry] {
        &self.code_entries
    }

    /// Final state layout consumed by every generated entry.
    pub fn layout(&self) -> &MemoryLayout {
        &self.layout
    }

    /// Source-independent instance hierarchy and signal metadata.
    pub fn reflection(&self) -> &DesignReflection {
        &self.reflection
    }

    /// Design metadata used to name saved state objects.
    pub(crate) fn state_design(&self) -> &crate::ir::RuntimeDesign {
        &self.design
    }

    /// Source-independent runtime diagnostics and combinational observers.
    pub(crate) fn runtime_schema(&self) -> &NativeRuntimeSchema {
        &self.runtime_schema
    }

    /// Whether the image's generated code and state layout use four-state data.
    pub fn four_state(&self) -> bool {
        self.options.four_state
    }

    /// Canonical event-domain topology used by the runtime scheduler.
    pub(crate) fn event_topology(&self) -> &celox_design::EventTopology<AbsoluteAddr> {
        &self.design.events
    }

    /// The storage a reflected signal shares with its aliases: its own, or
    /// for a clock or reset its canonical event domain's.
    pub fn signal_identity(
        &self,
        id: celox_runtime::ReflectionSignalId,
    ) -> Option<super::NativeSignalIdentity> {
        let signal = self.reflection.signal(id)?;
        Some(if signal.domain_kind == celox_design::DomainKind::Other {
            super::NativeSignalIdentity::State(signal.signal)
        } else {
            super::NativeSignalIdentity::Event(
                self.event_topology().canonical(signal.state_address),
            )
        })
    }

    /// Whether the design has process kernels, which only a timed
    /// [`crate::Simulation`] runs.
    pub fn has_processes(&self) -> bool {
        !self.runtime_schema.processes.is_empty()
    }

    /// The compiler-inserted override slots of a reflected signal, as
    /// (enable, value) pairs: setting the enable makes the design read the
    /// value instead of its drivers.
    pub fn force_slots(
        &self,
        id: celox_runtime::ReflectionSignalId,
    ) -> Vec<(SignalRef, SignalRef)> {
        self.runtime_schema
            .force_bindings
            .iter()
            .filter(|binding| binding.signal == id)
            .map(|binding| (binding.enabled, binding.value))
            .collect()
    }

    /// Every reflected signal stored at `identity`, in storage order.
    pub fn signal_aliases(&self, identity: super::NativeSignalIdentity) -> Vec<SignalRef> {
        let mut signals = self
            .reflection
            .signals()
            .iter()
            .enumerate()
            .filter_map(|(index, signal)| {
                (self.signal_identity(celox_runtime::ReflectionSignalId(index as u32))
                    == Some(identity))
                .then_some(signal.signal)
            })
            .collect::<Vec<_>>();
        signals.sort_unstable();
        signals.dedup();
        signals
    }

    /// Reconstruct the source-independent runtime metadata retained by this
    /// image. No frontend parsing or SIR/layout work is needed on the
    /// execution side of a host-codegen workflow.
    pub(crate) fn runtime_program(&self) -> crate::ir::RuntimeProgram {
        crate::ir::RuntimeProgram {
            design: self.design.clone(),
            rtl_accesses: self.runtime_schema.rtl_accesses.iter().copied().collect(),
            runtime_schema: RuntimeSchema {
                runtime_errors: self.runtime_schema.runtime_errors.clone(),
                runtime_event_sites: self.runtime_schema.runtime_event_sites.clone(),
                extern_functions: self.runtime_schema.extern_functions.clone(),
                comb_observers: self.runtime_schema.comb_observers.clone(),
                testbench_read_roots: self.runtime_schema.testbench_read_roots.clone(),
                rtl_writes: self.runtime_schema.rtl_writes.clone(),
                comb_writes: self.runtime_schema.comb_writes.iter().copied().collect(),
                persistent_comb_state: self
                    .runtime_schema
                    .persistent_comb_state
                    .iter()
                    .copied()
                    .collect(),
                declaration_startup: self.runtime_schema.declaration_startup.clone(),
                dead_store_policy: self.runtime_schema.dead_store_policy,
                live_signals: self.runtime_schema.live_signals.iter().copied().collect(),
                processes: self.runtime_schema.processes.clone(),
                process_origins: self.runtime_schema.process_origins.clone(),
                process_frames: self.runtime_schema.process_frames.clone(),
                watches: self.runtime_schema.watches.clone(),
                occurrence_probes: self.runtime_schema.occurrence_probes.clone(),
                wait_sites: self.runtime_schema.wait_sites.clone(),
                nba_sites: self.runtime_schema.nba_sites.clone(),
                host_request_sites: self.runtime_schema.host_request_sites.clone(),
                active_commits: self.runtime_schema.active_commits.clone(),
                time_slots: self.runtime_schema.time_slots.clone(),
                sample_shadows: self.runtime_schema.sample_shadows.clone(),
                initial_copies: self.runtime_schema.initial_copies.clone(),
                scheduler: self.runtime_schema.scheduler.clone(),
                derived_edges: self.runtime_schema.derived_edges.clone(),
                settle_partitions: None,
            },
            testbench: self.testbench.clone(),
        }
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        self.design
            .validate()
            .map_err(|error| format!("invalid runtime design: {error}"))?;
        self.reflection
            .validate()
            .map_err(|error| format!("invalid design reflection: {error}"))?;
        for binding in &self.runtime_schema.force_bindings {
            let target = self
                .reflection
                .signal(binding.signal)
                .ok_or("force target is not a reflected signal")?;
            if binding.enabled.width != 1
                || binding.enabled.is_4state
                || binding.value.width != target.signal.width
            {
                return Err("invalid force control shape".into());
            }
            for control in [binding.enabled, binding.value] {
                if control.array_layout.is_some() {
                    return Err("force controls must be packed".into());
                }
                let size = control
                    .width
                    .div_ceil(8)
                    .checked_mul(if control.is_4state { 2 } else { 1 })
                    .ok_or("force control size overflows")?;
                if control
                    .offset
                    .checked_add(size)
                    .is_none_or(|end| end > self.native_memory_size)
                {
                    return Err("force control exceeds native memory".into());
                }
            }
        }
        if self.code.is_empty() {
            return Err("code image is empty".into());
        }
        let mut entry_offsets = HashSet::default();
        let mut previous_end = 0usize;
        for entry in &self.code_entries {
            if !entry.offset.is_multiple_of(NATIVE_CODE_ENTRY_ALIGNMENT) {
                return Err(format!("entry `{}` is not aligned", entry.name));
            }
            if entry.size == 0 {
                return Err(format!("entry `{}` is empty", entry.name));
            }
            let end = entry
                .offset
                .checked_add(entry.size)
                .ok_or_else(|| format!("entry `{}` range overflows", entry.name))?;
            if entry.offset < previous_end || end > self.code.len() {
                return Err(format!("entry `{}` is outside the code image", entry.name));
            }
            if !entry_offsets.insert(entry.offset) {
                return Err(format!("entry `{}` duplicates an offset", entry.name));
            }
            previous_end = end;
        }
        if !entry_offsets.contains(&self.comb_offset) {
            return Err("eval_comb offset does not name an image entry".into());
        }
        for &offset in &self.runtime_schema.native_kernel_offsets {
            if !entry_offsets.contains(&offset) {
                return Err("native kernel offset does not name an image entry".into());
            }
        }
        if self
            .comb_unit_offsets
            .iter()
            .any(|offset| !entry_offsets.contains(offset))
        {
            return Err("a combinational unit offset does not name an image entry".into());
        }
        if self
            .process_offsets
            .iter()
            .any(|offset| !entry_offsets.contains(offset))
        {
            return Err("a process kernel offset does not name an image entry".into());
        }
        // A generated scheduler unit follows the kernels.
        let scheduler_units = self
            .runtime_schema
            .scheduler
            .as_ref()
            .and_then(|scheduler| scheduler.unit)
            .map_or(0, |unit| {
                if unit == self.runtime_schema.processes.len() {
                    1
                } else {
                    usize::MAX
                }
            });
        // The derived-edge units follow the kernels and the scheduler.
        let derived_units = 2 * self.runtime_schema.derived_edges.len();
        let derived_in_range = self.runtime_schema.derived_edges.iter().all(|units| {
            units.rising < self.process_offsets.len() && units.falling < self.process_offsets.len()
        });
        if !derived_in_range
            || self.process_offsets.len()
                != self
                    .runtime_schema
                    .processes
                    .len()
                    .saturating_add(scheduler_units)
                    .saturating_add(derived_units)
        {
            return Err("process kernels and their control slots disagree".into());
        }
        if self.runtime_schema.process_frames.len() != self.runtime_schema.processes.len() {
            return Err("process kernels and their activation frames disagree".into());
        }
        for (slots, frame) in self
            .runtime_schema
            .processes
            .iter()
            .zip(&self.runtime_schema.process_frames)
        {
            let Some(frame) = frame else {
                continue;
            };
            let mut addresses = HashSet::default();
            if ![slots.resume, slots.status, slots.delay, frame.random]
                .iter()
                .all(|slot| frame.private.contains(slot))
                || frame.private.iter().chain(&frame.locals).any(|slot| {
                    !self.design.state_objects.contains_key(slot) || !addresses.insert(*slot)
                })
            {
                return Err("invalid process activation frame storage".into());
            }
            let mut entries = HashSet::default();
            for probe in &frame.event_probes {
                if probe.resume == 0
                    || probe.resume as usize >= frame.resume_count
                    || !entries.insert(probe.resume)
                    || !frame.private.contains(&probe.result)
                    || !probe.scratch.contains(&probe.result)
                    || probe
                        .scratch
                        .iter()
                        .any(|slot| !frame.private.contains(slot))
                    || probe.inputs.iter().any(|slot| !frame.locals.contains(slot))
                    || probe.inputs.iter().collect::<HashSet<_>>().len() != probe.inputs.len()
                {
                    return Err("invalid process event-expression probe".into());
                }
            }
        }
        if self.required_native_features & !KNOWN_NATIVE_FEATURES != 0 {
            return Err("native image contains unknown feature requirements".into());
        }
        if let Some(lanes) = &self.lanes {
            lanes.validate(&entry_offsets, &self.event_map)?;
        }
        for symbol in &self.symbols {
            let end = symbol
                .offset
                .checked_add(symbol.size)
                .ok_or_else(|| format!("symbol `{}` range overflows", symbol.name))?;
            if symbol.size == 0 || end > self.code.len() {
                return Err(format!(
                    "symbol `{}` is outside the code image",
                    symbol.name
                ));
            }
        }
        for event in self
            .event_map
            .values()
            .chain(self.eval_only_event_map.values())
            .chain(self.apply_event_map.values())
            .chain(self.id_to_event.iter())
        {
            if !entry_offsets.contains(&event.func_offset)
                || event
                    .comb_apply_offset
                    .is_some_and(|offset| !entry_offsets.contains(&offset))
            {
                return Err(format!(
                    "event {} references a missing image entry",
                    event.id
                ));
            }
        }
        let semantic_size = self
            .layout
            .merged_total_size
            .checked_add(self.layout.triggered_bits_total_size)
            .ok_or_else(|| "semantic memory size overflows".to_string())?;
        if let Some(trace) = &self.layout.trace {
            let metadata_start = self
                .layout
                .triggered_bits_offset
                .checked_add(self.layout.triggered_bits_total_size)
                .ok_or_else(|| "trace metadata offset overflows".to_string())?;
            if !trace.validate(
                self.layout.total_size,
                metadata_start,
                self.layout.scratch_base_offset,
            ) {
                return Err("invalid waveform activity layout".into());
            }
        }
        if self.native_memory_size < semantic_size {
            return Err("native memory is smaller than the semantic state".into());
        }
        for &(offset, size) in &self.four_state_inits {
            let end = size
                .checked_mul(2)
                .and_then(|size| offset.checked_add(size))
                .ok_or_else(|| "four-state initialization range overflows".to_string())?;
            if end > self.native_memory_size {
                return Err("four-state initialization exceeds native memory".into());
            }
        }
        Ok(())
    }
}

// ────────────────────────────────────────────────────────────────
// Compilation
// ────────────────────────────────────────────────────────────────

fn build_force_bindings(
    program: &crate::ir::RuntimeProgram,
    layout: &crate::backend::MemoryLayout,
    paths: &[String],
) -> Result<Vec<NativeForceBinding>, SimulatorError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let reflection = program.build_design_reflection(layout);
    let signal_ref = |addr: &AbsoluteAddr| SignalRef {
        offset: layout.offsets[addr],
        width: layout.widths[addr],
        is_4state: layout.is_4states[addr],
        array_layout: None,
    };
    let mut result = Vec::new();
    for path in paths {
        let (index, target) = reflection
            .signals()
            .iter()
            .enumerate()
            .find(|(_, signal)| signal.full_name == *path)
            .ok_or_else(|| {
                codegen_err(CodegenError::message(format!(
                    "unknown force signal `{path}`"
                )))
            })?;
        if !target.unpacked_dims.is_empty() || target.domain_kind != celox_design::DomainKind::Other
        {
            return Err(codegen_err(CodegenError::message(format!(
                "force signal `{path}` must be a packed non-clock variable"
            ))));
        }
        let mut found = false;
        for instance in program.design.instances() {
            for address in instance.state_addresses() {
                let var = program.design.variable(address).unwrap();
                if instance.resolves_path_to(&var.path, *address)
                    && var.path.len() > 1
                    && var.var_kind.is_port()
                    && layout.offsets[address] == target.signal.offset
                {
                    return Err(codegen_message(format!(
                        "force signal `{path}` is a structured port alias; select the owning interface variable"
                    )));
                }
                if layout.offsets[address] != target.signal.offset
                    || layout.widths[address] != target.signal.width
                {
                    continue;
                }
                let flat_name = var.path.join("$");
                let control = |prefix: &str| {
                    instance.state_addresses().iter().find(|addr| {
                        program.design.variable(addr).unwrap().path
                            == [format!("{prefix}{flat_name}")]
                    })
                };
                if let (Some(enabled), Some(value)) =
                    (control("@force-enable:"), control("@force-value:"))
                {
                    result.push(NativeForceBinding {
                        signal: celox_runtime::ReflectionSignalId(index as u32),
                        enabled: signal_ref(enabled),
                        value: signal_ref(value),
                    });
                    found = true;
                }
            }
        }
        if !found {
            return Err(codegen_err(CodegenError::message(format!(
                "no force instrumentation for `{path}`"
            ))));
        }
    }
    Ok(result)
}

fn codegen_err(error: CodegenError) -> SimulatorError {
    error.into()
}

pub(super) fn codegen_message(message: impl Into<String>) -> SimulatorError {
    codegen_err(CodegenError::message(message))
}

pub(super) struct CompiledNativeFunction {
    code: Vec<u8>,
    symbols: Vec<jit_mem::JitSymbol>,
    trace: Option<emit::NativeFunctionTrace>,
    pub(super) required_state_size: usize,
    pub(super) required_native_features: u8,
}

#[cfg_attr(
    any(
        all(feature = "arm64-codegen", not(target_arch = "aarch64")),
        all(feature = "x86_64-codegen", not(target_arch = "x86_64"))
    ),
    allow(dead_code)
)]
pub(crate) struct NativeCodegenTrace {
    pub optimized_sir: String,
    pub mir: String,
    pub reactive_graph: String,
    pub state_layout: String,
}

/// Optimization context of a merged native function. Diagnostic labels do
/// not describe whether pending values can be produced outside this function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeMergeKind {
    /// A complete sequential group of execution units.
    Complete,
    /// Combinational settling followed by an FF event in the same function.
    FusedCombAndFf,
    /// One task of a partitioned kernel; other tasks may define pending state.
    Partial,
}

fn prepare_merged_sir(
    units: &[&crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    label: &str,
    merge_kind: NativeMergeKind,
    first_ff_unit: Option<usize>,
    diagnostics: &crate::optimizer::SirDiagnostics,
    cancel: Option<&CompileCancel>,
    baseline: bool,
) -> Result<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>, SimulatorError> {
    let verify_enabled = cfg!(debug_assertions) || diagnostics.verify_boundaries;
    let mut phase_start = diagnostics.pass_timing.then(crate::timing::now);
    let mut report_phase = |phase: &str| {
        if let Some(start) = phase_start {
            tracing::debug!(
                "[native-sir-timing] label={label} phase={phase} elapsed={:?}",
                start.elapsed()
            );
            phase_start = Some(crate::timing::now());
        }
    };
    if verify_enabled {
        for (unit_index, unit) in units.iter().enumerate() {
            if let Err(error) = unit.verify_result() {
                return Err(codegen_err(CodegenError::SirVerification {
                    phase: format!(
                        "invalid SIR before x86 source-unit merge: {label} source unit {unit_index}"
                    ),
                    source: error,
                }));
            }
        }
    }

    let (mut sir_eu, merge_provenance) = celox_sir::merge_sir_eu_refs_with_provenance(units);
    let boundaries = merge_provenance.unit_entries[1..].to_vec();
    let verify = |eu: &crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>,
                  phase: &'static str| {
        if verify_enabled {
            eu.verify_result().map_err(|source| {
                codegen_err(CodegenError::SirVerification {
                    phase: phase.to_string(),
                    source,
                })
            })
        } else {
            Ok(())
        }
    };

    verify(&sir_eu, "before x86 merged-SIR optimization")?;
    report_phase("merge");
    if cancelled(cancel) {
        return Err(cancelled_error());
    }
    // The baseline tier keeps the finalized source-unit order and every
    // state store; the optional cross-unit transforms belong to the
    // optimizing tier. ISel/legalize/regalloc/emit still run below.
    if baseline {
        return Ok(sir_eu);
    }
    if let Some(first_ff_unit) = first_ff_unit {
        let removed = crate::optimizer::sir::eliminate_unobserved_comb_state_stores(
            &mut sir_eu,
            &merge_provenance,
            first_ff_unit,
        )
        .map_err(|source| {
            codegen_err(CodegenError::Optimization {
                context: "comb/FF state-publication DSE",
                source,
            })
        })?;
        if removed != 0 {
            crate::optimizer::sir::remove_dead_sir_definitions(&mut sir_eu);
            verify(&sir_eu, "after comb/FF state-publication DSE")?;
        }
    }
    report_phase("comb_state_dse");
    if merge_kind == NativeMergeKind::FusedCombAndFf
        && crate::optimizer::sir::promote_fused_comb_static_slots(&mut sir_eu).map_err(
            |source| {
                codegen_err(CodegenError::Optimization {
                    context: "final fused comb StateSSA promotion",
                    source,
                })
            },
        )?
    {
        crate::optimizer::sir::remove_dead_sir_definitions(&mut sir_eu);
        verify(&sir_eu, "after final fused comb StateSSA promotion")?;
    }
    report_phase("comb_state_ssa");
    match merge_kind {
        NativeMergeKind::Partial => {
            crate::optimizer::sir::pass_eliminate_working_round_trip::eliminate_partial_working_round_trip(&mut sir_eu);
        }
        NativeMergeKind::Complete | NativeMergeKind::FusedCombAndFf => {
            crate::optimizer::sir::pass_eliminate_working_round_trip::eliminate_working_round_trip(
                &mut sir_eu,
                &boundaries,
            );
        }
    }
    verify(&sir_eu, "after x86 direct working rewrite")?;
    if cancelled(cancel) {
        return Err(cancelled_error());
    }
    let promoted_working =
        crate::optimizer::sir::promote_eval_apply_working_round_trips(&mut sir_eu);
    if promoted_working {
        verify(&sir_eu, "after x86 working StateSSA")?;
        crate::optimizer::sir::remove_dead_sir_definitions(&mut sir_eu);
        verify(&sir_eu, "after x86 working StateSSA DCE")?;
    }
    report_phase("working_state_ssa");
    if cancelled(cancel) {
        return Err(cancelled_error());
    }
    crate::optimizer::sir::optimize_native_merged_chain(
        &mut sir_eu,
        layout,
        four_state,
        merge_kind == NativeMergeKind::FusedCombAndFf,
        diagnostics,
        || cancelled(cancel),
    )
    .map_err(|source| {
        if source.kind() == celox_sir_opt::OptimizationErrorKind::Cancelled {
            codegen_err(CodegenError::Cancelled)
        } else {
            codegen_err(CodegenError::Optimization {
                context: "native merged-chain optimization",
                source,
            })
        }
    })?;
    verify(&sir_eu, "after x86 merged-chain cleanup")?;
    report_phase("merged_chain");
    Ok(sir_eu)
}

fn compile_units(
    units: &[crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    label: &str,
    x86_options: &crate::backend::X86BackendOptions,
    capture_trace: bool,
    diagnostics: &crate::optimizer::SirDiagnostics,
    cancel: Option<&CompileCancel>,
) -> Result<CompiledNativeFunction, SimulatorError> {
    let units = units.iter().collect::<Vec<_>>();
    compile_unit_refs(
        &units,
        layout,
        four_state,
        label,
        NativeMergeKind::Complete,
        None,
        x86_options,
        capture_trace,
        diagnostics,
        cancel,
    )
}

pub(super) fn compile_unit_refs(
    units: &[&crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    label: &str,
    merge_kind: NativeMergeKind,
    first_ff_unit: Option<usize>,
    x86_options: &crate::backend::X86BackendOptions,
    capture_trace: bool,
    diagnostics: &crate::optimizer::SirDiagnostics,
    cancel: Option<&CompileCancel>,
) -> Result<CompiledNativeFunction, SimulatorError> {
    compile_unit_refs_inspected(
        units,
        layout,
        four_state,
        label,
        merge_kind,
        first_ff_unit,
        x86_options,
        capture_trace,
        diagnostics,
        cancel,
        &mut |_| {},
    )
}

/// [`compile_unit_refs`] that shows the merged SIR, after every SIR rewrite
/// of the native pipeline, to `inspect_prepared` before instruction
/// selection. It is not called for an empty unit list.
#[allow(clippy::too_many_arguments)]
pub(super) fn compile_unit_refs_inspected(
    units: &[&crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    label: &str,
    merge_kind: NativeMergeKind,
    first_ff_unit: Option<usize>,
    x86_options: &crate::backend::X86BackendOptions,
    capture_trace: bool,
    diagnostics: &crate::optimizer::SirDiagnostics,
    cancel: Option<&CompileCancel>,
    inspect_prepared: &mut dyn FnMut(&crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>),
) -> Result<CompiledNativeFunction, SimulatorError> {
    if cancelled(cancel) {
        return Err(cancelled_error());
    }
    let timing = x86_options.diagnostics.phase_timing;
    if units.is_empty() {
        // Empty function: just return 0
        #[cfg(any(
            feature = "x86_64-codegen",
            all(target_arch = "x86_64", not(feature = "arm64-codegen"))
        ))]
        let (empty_result, empty_mir) = {
            let mut empty_func =
                super::mir::MFunction::new(super::mir::VRegAllocator::new(), vec![]);
            let mut block = super::mir::MBlock::new(super::mir::BlockId(0));
            block.push(super::mir::MInst::Return);
            empty_func.push_block(block);
            let empty_result = emit::emit(&empty_func, &regalloc::AssignmentMap::default(), 0)
                .map_err(|source| codegen_err(CodegenError::NativeEmission { source }))?;
            let empty_mir = empty_func.to_string();
            (empty_result, empty_mir)
        };
        #[cfg(any(
            feature = "arm64-codegen",
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        let (empty_result, empty_mir) = {
            let state_size = layout
                .merged_total_size
                .checked_add(layout.triggered_bits_total_size)
                .ok_or_else(|| {
                    codegen_err(CodegenError::NativeEmission {
                        source: emit::EmitError::Range("AArch64 simulation-state size overflow"),
                    })
                })?;
            let result = emit::emit_empty(state_size)
                .map_err(|source| codegen_err(CodegenError::NativeEmission { source }))?;
            (result, "bb0:\n  Return\n".to_string())
        };
        let trace = capture_trace.then(|| emit::NativeFunctionTrace {
            optimized_sir: "<empty native function>\n".into(),
            reactive_graph: String::new(),
            state_layout: String::new(),
            mir_before_regalloc: empty_mir.clone(),
            mir_after_late_memory_folds: empty_mir.clone(),
            mir_after_scheduling: empty_mir.clone(),
            mir_after_regalloc: empty_mir,
            register_assignment: String::new(),
            spill_frame_size: 0,
            disassembly: emit::disassemble(&empty_result.code[..empty_result.text_size], 0),
        });
        let symbols = perf_symbols_for_emit_result(label, &empty_result);
        return Ok(CompiledNativeFunction {
            code: empty_result.code,
            symbols,
            trace,
            required_state_size: empty_result.required_state_size as usize,
            #[cfg(any(
                feature = "x86_64-codegen",
                all(target_arch = "x86_64", not(feature = "arm64-codegen"))
            ))]
            required_native_features: empty_result.required_image_features,
            #[cfg(any(
                feature = "arm64-codegen",
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            required_native_features: 0,
        });
    }

    // Merge all EUs and compile the exact SIR/MIR function used at runtime.
    if timing {
        tracing::debug!(
            "[native-timing] compile_units start label={label} eus={}",
            units.len()
        );
    }
    let start = timing.then(crate::timing::now);
    let sir_eu = prepare_merged_sir(
        units,
        layout,
        four_state,
        label,
        merge_kind,
        first_ff_unit,
        diagnostics,
        cancel,
        x86_options.baseline,
    )?;
    inspect_prepared(&sir_eu);
    if cancelled(cancel) {
        return Err(cancelled_error());
    }
    let mut trace = capture_trace.then(emit::NativeFunctionTrace::default);
    #[cfg(any(
        feature = "x86_64-codegen",
        all(target_arch = "x86_64", not(feature = "arm64-codegen"))
    ))]
    let emit_result = emit::emit_owned_prepared_eu(
        sir_eu,
        layout,
        four_state,
        label,
        x86_options,
        trace.as_mut(),
        || cancelled(cancel),
    )
    .map_err(|source| {
        if matches!(source, emit::ChainedEmitError::Cancelled) {
            codegen_err(CodegenError::Cancelled)
        } else {
            codegen_err(CodegenError::NativePipeline { source })
        }
    })?;
    #[cfg(any(
        feature = "arm64-codegen",
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    let emit_result = emit::emit_prepared_eu_with_arena(
        &sir_eu,
        layout,
        four_state,
        label,
        x86_options.native_tick_loop,
        x86_options.arena_base,
        trace.as_mut(),
        || cancelled(cancel),
    )
    .map_err(|source| {
        if matches!(source, emit::ChainedEmitError::Cancelled) {
            codegen_err(CodegenError::Cancelled)
        } else {
            codegen_err(CodegenError::NativePipeline { source })
        }
    })?;
    if let Some(start) = start {
        tracing::debug!(
            "[native-timing] compile_units done label={label} bytes={} elapsed={:?}",
            emit_result.code.len(),
            start.elapsed()
        );
    }
    let symbols = perf_symbols_for_emit_result(label, &emit_result);
    let required_state_size = emit_result.required_state_size as usize;
    Ok(CompiledNativeFunction {
        code: emit_result.code,
        symbols,
        trace,
        required_state_size,
        #[cfg(any(
            feature = "x86_64-codegen",
            all(target_arch = "x86_64", not(feature = "arm64-codegen"))
        ))]
        required_native_features: emit_result.required_image_features,
        #[cfg(any(
            feature = "arm64-codegen",
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        required_native_features: 0,
    })
}

fn perf_symbols_for_emit_result(label: &str, result: &emit::EmitResult) -> Vec<jit_mem::JitSymbol> {
    let code_len = result.text_size;
    if result.block_offsets.is_empty() {
        return Vec::new();
    }

    let mut blocks = result.block_offsets.clone();
    blocks.sort_by_key(|(_, offset)| *offset);

    let mut symbols = Vec::with_capacity(blocks.len() + 2);
    let first_offset = blocks[0].1 as usize;
    if first_offset > 0 {
        symbols.push(jit_mem::JitSymbol {
            offset: 0,
            size: first_offset,
            name: format!("{label}.prologue"),
        });
    }

    for (idx, (block_id, offset)) in blocks.iter().enumerate() {
        let start = *offset as usize;
        let end = blocks
            .get(idx + 1)
            .map(|(_, next)| *next as usize)
            .unwrap_or(code_len);
        if end > start {
            symbols.push(jit_mem::JitSymbol {
                offset: start,
                size: end - start,
                name: format!("{label}.bb{}", block_id.0),
            });
        }
    }

    symbols
}

const NATIVE_CODE_ENTRY_ALIGNMENT: usize = 16;

pub(super) fn append_native_code(
    image: &mut Vec<u8>,
    entries: &mut Vec<NativeCodeEntry>,
    image_symbols: &mut Vec<NativeCodeSymbol>,
    name: String,
    compiled: &CompiledNativeFunction,
) -> Result<usize, SimulatorError> {
    let offset = image
        .len()
        .checked_add(NATIVE_CODE_ENTRY_ALIGNMENT - 1)
        .map(|value| value & !(NATIVE_CODE_ENTRY_ALIGNMENT - 1))
        .ok_or_else(|| codegen_message("packed native code image alignment overflow"))?;
    image.resize(offset, 0);
    let end = offset
        .checked_add(compiled.code.len())
        .ok_or_else(|| codegen_message("packed native code image size overflow"))?;
    image.extend_from_slice(&compiled.code);

    if compiled.symbols.is_empty() {
        image_symbols.push(NativeCodeSymbol {
            offset,
            size: compiled.code.len(),
            name: name.clone(),
        });
    } else {
        for symbol in &compiled.symbols {
            let symbol_end = symbol
                .offset
                .checked_add(symbol.size)
                .ok_or_else(|| codegen_message("native function symbol range overflow"))?;
            if symbol_end > compiled.code.len() {
                return Err(codegen_message(format!(
                    "native function symbol `{}` exceeds its emitted code",
                    symbol.name
                )));
            }
            image_symbols.push(NativeCodeSymbol {
                offset: offset + symbol.offset,
                size: symbol.size,
                name: format!("{name}.{}", symbol.name),
            });
        }
    }

    entries.push(NativeCodeEntry {
        name,
        offset,
        size: end - offset,
    });
    Ok(offset)
}

pub(super) fn native_function_at(
    image: &jit_mem::JitCode,
    offset: usize,
) -> Result<NativeSimFunc, SimulatorError> {
    let ptr = image
        .entry_ptr(offset)
        .ok_or_else(|| codegen_message("native function entry exceeds packed code image"))?;
    // Safety: `offset` was returned by `append_native_code` for a complete
    // function emitted with `NativeSimFunc`'s target ABI. `image` owns the
    // executable allocation for at least as long as the returned pointer.
    Ok(unsafe { std::mem::transmute::<*const u8, NativeSimFunc>(ptr) })
}

struct NativeCompileTask<'a> {
    units: Vec<&'a crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
    label: &'static str,
    merge_kind: NativeMergeKind,
    first_ff_unit: Option<usize>,
    bindings: Vec<String>,
}

type NativeTaskBindings = HashMap<(&'static str, AbsoluteAddr), usize>;

type NativeUnit = crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>;

/// Classes of structurally equal execution units. Each distinct unit is
/// hashed once and compared only against the units of its hash, so tasks
/// whose unit lists share whole programs, such as every unfused comb-apply
/// task starting with all of `eval_comb`, are told apart by their class
/// lists instead of by comparing the programs again for each trigger.
#[derive(Default)]
struct NativeUnitClasses<'a> {
    by_unit: HashMap<*const NativeUnit, usize>,
    by_hash: HashMap<u64, Vec<(&'a NativeUnit, usize)>>,
    count: usize,
}

impl<'a> NativeUnitClasses<'a> {
    fn class(&mut self, unit: &'a NativeUnit) -> usize {
        if let Some(class) = self.by_unit.get(&std::ptr::from_ref(unit)) {
            return *class;
        }
        let bucket = self.by_hash.entry(native_unit_hash(unit)).or_default();
        let class = match bucket.iter().find(|(other, _)| *other == unit) {
            Some((_, class)) => *class,
            None => {
                let class = self.count;
                self.count += 1;
                bucket.push((unit, class));
                class
            }
        };
        self.by_unit.insert(std::ptr::from_ref(unit), class);
        class
    }

    fn classes(&mut self, units: &[&'a NativeUnit]) -> Vec<usize> {
        units.iter().map(|unit| self.class(unit)).collect()
    }
}

/// A hash of `unit` that equal units share, independent of the iteration
/// order of its maps.
fn native_unit_hash(unit: &NativeUnit) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = fxhash::FxHasher::default();
    unit.entry_block_id.hash(&mut hasher);
    let mut blocks = unit.blocks.iter().collect::<Vec<_>>();
    blocks.sort_unstable_by_key(|(id, _)| **id);
    for (id, block) in blocks {
        id.hash(&mut hasher);
        block.id.hash(&mut hasher);
        block.params.hash(&mut hasher);
        block.instructions.hash(&mut hasher);
        block.terminator.hash(&mut hasher);
    }
    let mut registers = unit.register_map.iter().collect::<Vec<_>>();
    registers.sort_unstable_by_key(|(id, _)| **id);
    registers.hash(&mut hasher);
    hasher.finish()
}

fn collect_ff_compile_tasks(
    sir: &LaidOutProgram,
) -> (Vec<NativeCompileTask<'_>>, NativeTaskBindings) {
    let mut tasks = Vec::new();
    let mut task_bindings = HashMap::default();
    let mut classes = NativeUnitClasses::default();
    // Edge tasks of every label share one index: a task is reused for any
    // edge trigger with equal units.
    let mut ff_tasks = HashMap::default();
    for (ff_map, label) in [
        (&sir.sir.eval_apply_ffs, "eval_apply_ff"),
        (&sir.sir.eval_only_ffs, "eval_only_ff"),
        (&sir.sir.apply_ffs, "apply_ff"),
    ] {
        collect_ff_compile_tasks_from(
            sir,
            ff_map,
            label,
            &mut classes,
            &mut ff_tasks,
            &mut tasks,
            &mut task_bindings,
        );
    }
    collect_comb_apply_compile_tasks(sir, &mut classes, &mut tasks, &mut task_bindings);
    (tasks, task_bindings)
}

fn collect_comb_apply_compile_tasks<'a>(
    sir: &'a LaidOutProgram,
    classes: &mut NativeUnitClasses<'a>,
    tasks: &mut Vec<NativeCompileTask<'a>>,
    task_bindings: &mut NativeTaskBindings,
) {
    const LABEL: &str = "eval_comb_apply_ff";
    let mut comb_tasks = HashMap::<(Option<usize>, Vec<usize>), usize>::default();
    for addr in sir.sir.eval_apply_ffs.keys() {
        // A shared tick calls the comb and FF functions compiled anyway.
        let (unit_refs, first_ff_unit) = match sir.sir.comb_apply(addr) {
            Some(celox_sir::CombApply::Fused(fused_units)) => {
                (fused_units.iter().collect::<Vec<_>>(), None)
            }
            Some(celox_sir::CombApply::Merged { comb, ff }) => {
                let mut unit_refs = comb.iter().collect::<Vec<_>>();
                let first_ff_unit =
                    (!unit_refs.is_empty() && !ff.is_empty()).then_some(unit_refs.len());
                unit_refs.extend(ff);
                (unit_refs, first_ff_unit)
            }
            Some(celox_sir::CombApply::Shared) | None => continue,
        };
        let binding = format!("{LABEL} trigger={}", sir.get_path(addr));
        let key = (first_ff_unit, classes.classes(&unit_refs));
        let index = if let Some(&index) = comb_tasks.get(&key) {
            tasks[index].bindings.push(binding);
            index
        } else {
            let index = tasks.len();
            tasks.push(NativeCompileTask {
                units: unit_refs,
                label: LABEL,
                merge_kind: NativeMergeKind::FusedCombAndFf,
                first_ff_unit,
                bindings: vec![binding],
            });
            comb_tasks.insert(key, index);
            index
        };
        task_bindings.insert((LABEL, *addr), index);
    }
}

fn collect_ff_compile_tasks_from<'a>(
    sir: &LaidOutProgram,
    ff_map: &'a HashMap<
        AbsoluteAddr,
        Vec<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
    >,
    label: &'static str,
    classes: &mut NativeUnitClasses<'a>,
    ff_tasks: &mut HashMap<Vec<usize>, usize>,
    tasks: &mut Vec<NativeCompileTask<'a>>,
    task_bindings: &mut NativeTaskBindings,
) {
    for (addr, units) in ff_map {
        let unit_refs = units.iter().collect::<Vec<_>>();
        let binding = format!("{label} trigger={}", sir.get_path(addr));
        let key = classes.classes(&unit_refs);
        let index = if let Some(&index) = ff_tasks.get(&key) {
            tasks[index].bindings.push(binding);
            index
        } else {
            let index = tasks.len();
            tasks.push(NativeCompileTask {
                units: unit_refs,
                label,
                merge_kind: NativeMergeKind::Complete,
                first_ff_unit: None,
                bindings: vec![binding],
            });
            ff_tasks.insert(key, index);
            index
        };
        task_bindings.insert((label, *addr), index);
    }
}

fn append_native_function_trace(
    optimized_sir: &mut String,
    mir: &mut String,
    reactive_graph: &mut String,
    state_layout: &mut String,
    name: &str,
    bindings: &[String],
    trace: &emit::NativeFunctionTrace,
) {
    let mut bindings = bindings.to_vec();
    bindings.sort();
    bindings.dedup();

    optimized_sir.push_str(&format!("=== Native function {name} ===\n"));
    if !bindings.is_empty() {
        optimized_sir.push_str("Bindings:\n");
        for binding in &bindings {
            optimized_sir.push_str(&format!("  {binding}\n"));
        }
    }
    optimized_sir.push_str(&trace.optimized_sir);
    if !trace.optimized_sir.ends_with('\n') {
        optimized_sir.push('\n');
    }
    optimized_sir.push('\n');

    mir.push_str(&format!("=== Native function {name} ===\n"));
    if !bindings.is_empty() {
        mir.push_str("Bindings:\n");
        for binding in &bindings {
            mir.push_str(&format!("  {binding}\n"));
        }
    }
    mir.push_str("--- MIR after main optimization, before regalloc-owned late folds ---\n");
    mir.push_str(&trace.mir_before_regalloc);
    if !trace.mir_before_regalloc.ends_with('\n') {
        mir.push('\n');
    }
    mir.push_str("--- MIR after late memory folds, before allocation-owned scheduling ---\n");
    mir.push_str(&trace.mir_after_late_memory_folds);
    if !trace.mir_after_late_memory_folds.ends_with('\n') {
        mir.push('\n');
    }
    mir.push_str("--- MIR after allocation-owned scheduling, before spill reconstruction ---\n");
    mir.push_str(&trace.mir_after_scheduling);
    if !trace.mir_after_scheduling.ends_with('\n') {
        mir.push('\n');
    }
    mir.push_str("--- MIR after register allocation and post-RA peepholes ---\n");
    mir.push_str(&trace.mir_after_regalloc);
    if !trace.mir_after_regalloc.ends_with('\n') {
        mir.push('\n');
    }
    mir.push_str(&format!("Spill frame: {} bytes\n", trace.spill_frame_size));
    mir.push_str("Register assignment:\n");
    mir.push_str(&trace.register_assignment);
    #[cfg(any(
        feature = "x86_64-codegen",
        all(target_arch = "x86_64", not(feature = "arm64-codegen"))
    ))]
    mir.push_str("x86-64 disassembly of emitted function:\n");
    #[cfg(any(
        feature = "arm64-codegen",
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    mir.push_str("AArch64 disassembly of emitted function:\n");
    mir.push_str(&trace.disassembly);
    if !trace.disassembly.ends_with('\n') {
        mir.push('\n');
    }
    mir.push('\n');

    if !trace.reactive_graph.is_empty() {
        reactive_graph.push_str(&format!("=== Native function {name} ===\n"));
        if !bindings.is_empty() {
            reactive_graph.push_str("Bindings:\n");
            for binding in &bindings {
                reactive_graph.push_str(&format!("  {binding}\n"));
            }
        }
        reactive_graph.push_str(&trace.reactive_graph);
        if !trace.reactive_graph.ends_with('\n') {
            reactive_graph.push('\n');
        }
        reactive_graph.push('\n');
    }

    if !trace.state_layout.is_empty() {
        state_layout.push_str(&format!("=== Native function {name} ===\n"));
        if !bindings.is_empty() {
            state_layout.push_str("Bindings:\n");
            for binding in &bindings {
                state_layout.push_str(&format!("  {binding}\n"));
            }
        }
        state_layout.push_str(&trace.state_layout);
        if !trace.state_layout.ends_with('\n') {
            state_layout.push('\n');
        }
        state_layout.push('\n');
    }
}

fn format_native_codegen_trace(
    comb: &CompiledNativeFunction,
    ff_codes: &HashMap<usize, CompiledNativeFunction>,
    tasks: &[NativeCompileTask<'_>],
) -> NativeCodegenTrace {
    let mut optimized_sir = String::from("=== Optimized SIR used by native emission ===\n");
    let mut mir = String::from("=== MIR used by native emission ===\n");
    let mut reactive_graph = String::from("=== Reactive clock-event projection oracle ===\n");
    let mut state_layout =
        String::from("=== Profile-selected native state-layout feasibility ===\n");
    append_native_function_trace(
        &mut optimized_sir,
        &mut mir,
        &mut reactive_graph,
        &mut state_layout,
        "eval_comb",
        &[],
        comb.trace
            .as_ref()
            .expect("explicit native trace must capture eval_comb"),
    );

    let mut ff_entries = ff_codes
        .keys()
        .map(|&task_id| {
            let task = &tasks[task_id];
            let mut sort_key = task.bindings.clone();
            sort_key.sort();
            (sort_key, task_id, task)
        })
        .collect::<Vec<_>>();
    ff_entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut label_indices = HashMap::<&str, usize>::default();
    for (_, task_id, task) in ff_entries {
        let index = label_indices.entry(task.label).or_default();
        let name = format!("{}[{index}]", task.label);
        *index += 1;
        append_native_function_trace(
            &mut optimized_sir,
            &mut mir,
            &mut reactive_graph,
            &mut state_layout,
            &name,
            &task.bindings,
            ff_codes[&task_id]
                .trace
                .as_ref()
                .expect("explicit native trace must capture every FF function"),
        );
    }
    NativeCodegenTrace {
        optimized_sir,
        mir,
        reactive_graph,
        state_layout,
    }
}

fn offset_registers(offset: &SIROffset, registers: &mut Vec<RegisterId>) {
    match offset {
        SIROffset::Dynamic(register) => registers.push(*register),
        SIROffset::Element {
            index,
            dynamic_bit_offset,
            ..
        } => {
            registers.push(*index);
            registers.extend(dynamic_bit_offset);
        }
        SIROffset::Static(_) | SIROffset::PackedElements { .. } => {}
    }
}

fn instruction_registers<A>(instruction: &SIRInstruction<A>) -> Vec<RegisterId> {
    let mut registers = Vec::new();
    instruction.for_each_use(|register| registers.push(register));
    registers
}

fn comb_block_execution_order<A>(unit: &ExecutionUnit<A>) -> Vec<BlockId> {
    fn visit<A>(
        unit: &ExecutionUnit<A>,
        block_id: BlockId,
        visited: &mut HashSet<BlockId>,
        postorder: &mut Vec<BlockId>,
    ) {
        if !visited.insert(block_id) {
            return;
        }
        for successor in celox_sir::cfg::terminator_successors(&unit.blocks[&block_id].terminator) {
            visit(unit, successor, visited, postorder);
        }
        postorder.push(block_id);
    }

    let mut visited = HashSet::default();
    let mut postorder = Vec::with_capacity(unit.blocks.len());
    visit(unit, unit.entry_block_id, &mut visited, &mut postorder);
    postorder.reverse();
    postorder
}

fn is_comb_runtime_effect(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    instruction.is_host_interaction()
}

fn interleave_comb_runtime_effects(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    ordered_stores: &[(BlockId, usize)],
    store_units: Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
) -> Vec<ExecutionUnit<RegionedAbsoluteAddr>> {
    let ordered_sites = comb_block_execution_order(unit)
        .into_iter()
        .flat_map(|block_id| {
            (0..unit.blocks[&block_id].instructions.len()).map(move |index| (block_id, index))
        })
        .collect::<Vec<_>>();
    let positions = ordered_sites
        .iter()
        .enumerate()
        .map(|(position, &site)| (site, position))
        .collect::<HashMap<_, _>>();
    let mut effect_groups = vec![Vec::new(); ordered_stores.len() + 1];
    for site in ordered_sites {
        if !is_comb_runtime_effect(&unit.blocks[&site.0].instructions[site.1]) {
            continue;
        }
        let boundary = ordered_stores
            .iter()
            .filter(|store| positions[store] < positions[&site])
            .count();
        effect_groups[boundary].push(site);
    }
    if effect_groups.iter().all(Vec::is_empty) {
        return store_units;
    }

    let mut result = Vec::with_capacity(store_units.len() + effect_groups.len());
    let mut stores = store_units.into_iter();
    for (boundary, group) in effect_groups.into_iter().enumerate() {
        if !group.is_empty() {
            let group = group.into_iter().collect::<HashSet<_>>();
            let mut events = unit.clone();
            for (block_id, block) in &mut events.blocks {
                block.instructions = std::mem::take(&mut block.instructions)
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, instruction)| {
                        let site = (*block_id, index);
                        if matches!(
                            instruction,
                            SIRInstruction::Store(..) | SIRInstruction::Commit(..)
                        ) {
                            None
                        } else if is_comb_runtime_effect(&instruction) {
                            group.contains(&site).then_some(instruction)
                        } else {
                            Some(instruction)
                        }
                    })
                    .collect();
            }
            result.push(events);
        }
        if boundary < ordered_stores.len() {
            result.push(stores.next().unwrap());
        }
    }
    result
}

fn split_comb_execution_unit(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> Vec<ExecutionUnit<RegionedAbsoluteAddr>> {
    if unit.blocks.len() != 1 {
        let definitions = unit
            .blocks
            .iter()
            .flat_map(|(block_id, block)| {
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .filter_map(|(index, instruction)| {
                        instruction
                            .defined_register()
                            .map(|register| (register, (*block_id, index)))
                    })
            })
            .collect::<HashMap<_, _>>();
        let store_sites = comb_block_execution_order(unit)
            .into_iter()
            .flat_map(|block_id| {
                let block = &unit.blocks[&block_id];
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .filter(|(_, instruction)| {
                        matches!(
                            instruction,
                            SIRInstruction::Store(..) | SIRInstruction::Commit(..)
                        )
                    })
                    .map(move |(index, _)| (block_id, index))
            })
            .collect::<Vec<_>>();
        if store_sites.is_empty() {
            return vec![unit.clone()];
        }

        let instruction_at = |site: (BlockId, usize)| &unit.blocks[&site.0].instructions[site.1];
        let register_dependencies = store_sites
            .iter()
            .copied()
            .map(|store_site| {
                let mut dependencies = HashSet::default();
                let mut pending = instruction_registers(instruction_at(store_site));
                while let Some(register) = pending.pop() {
                    if !dependencies.insert(register) {
                        continue;
                    }
                    if let Some(&definition) = definitions.get(&register) {
                        pending.extend(instruction_registers(instruction_at(definition)));
                    }
                }
                (store_site, dependencies)
            })
            .collect::<HashMap<_, _>>();
        let store_source = |site| match instruction_at(site) {
            SIRInstruction::Store(_, _, _, source, _, _) => Some(*source),
            _ => None,
        };
        let mut remaining = store_sites;
        let mut ordered_stores = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .position(|candidate| {
                    let candidate_source = store_source(*candidate);
                    !remaining.iter().any(|predecessor| {
                        if predecessor == candidate {
                            return false;
                        }
                        store_source(*predecessor).is_some_and(|source| {
                            Some(source) != candidate_source
                                && register_dependencies[candidate].contains(&source)
                        })
                    })
                })
                .unwrap_or(0);
            ordered_stores.push(remaining.remove(next));
        }

        let split = ordered_stores
            .iter()
            .enumerate()
            .map(|(order, &target)| {
                let reloads = ordered_stores[..order]
                    .iter()
                    .filter_map(|&prior_site| {
                        let SIRInstruction::Store(address, offset, bits, source, _, _) =
                            instruction_at(prior_site)
                        else {
                            return None;
                        };
                        (register_dependencies[&target].contains(source)
                            && store_source(target) != Some(*source)
                            && unit.register_map[source].width() == *bits)
                            .then(|| (*source, (*address, offset.clone(), *bits)))
                    })
                    .collect::<HashMap<_, _>>();
                let mut extracted = unit.clone();
                for (block_id, block) in &mut extracted.blocks {
                    block.instructions = std::mem::take(&mut block.instructions)
                        .into_iter()
                        .enumerate()
                        .filter_map(|(index, instruction)| {
                            let site = (*block_id, index);
                            match instruction {
                                _ if instruction.memory_write().is_some() => {
                                    (site == target).then_some(instruction)
                                }
                                _ if instruction.has_side_effects() => None,
                                _ => {
                                    if let Some(register) = instruction.defined_register()
                                        && let Some((address, offset, bits)) =
                                            reloads.get(&register)
                                    {
                                        Some(SIRInstruction::Load(
                                            register,
                                            *address,
                                            offset.clone(),
                                            *bits,
                                        ))
                                    } else {
                                        Some(instruction)
                                    }
                                }
                            }
                        })
                        .collect();
                }
                extracted
            })
            .collect::<Vec<_>>();

        return interleave_comb_runtime_effects(unit, &ordered_stores, split);
    }
    let block = &unit.blocks[&unit.entry_block_id];
    if !block.params.is_empty() || block.terminator != SIRTerminator::Return {
        return vec![unit.clone()];
    }

    let definitions = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            instruction
                .defined_register()
                .map(|register| (register, index))
        })
        .collect::<HashMap<_, _>>();
    let store_indices = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            matches!(
                instruction,
                SIRInstruction::Store(..) | SIRInstruction::Commit(..)
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    if store_indices.is_empty() {
        return vec![unit.clone()];
    }

    let register_dependencies = store_indices
        .iter()
        .copied()
        .map(|store_index| {
            let mut dependencies = HashSet::default();
            let mut pending = instruction_registers(&block.instructions[store_index]);
            while let Some(register) = pending.pop() {
                if !dependencies.insert(register) {
                    continue;
                }
                if let Some(&definition) = definitions.get(&register) {
                    pending.extend(instruction_registers(&block.instructions[definition]));
                }
            }
            (store_index, dependencies)
        })
        .collect::<HashMap<_, _>>();
    let store_source = |index| match &block.instructions[index] {
        SIRInstruction::Store(_, _, _, source, _, _) => Some(*source),
        _ => None,
    };
    let mut remaining = store_indices.clone();
    let mut ordered_stores = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let next = remaining
            .iter()
            .position(|candidate| {
                let candidate_source = store_source(*candidate);
                !remaining.iter().any(|predecessor| {
                    if predecessor == candidate {
                        return false;
                    }
                    store_source(*predecessor).is_some_and(|source| {
                        Some(source) != candidate_source
                            && register_dependencies[candidate].contains(&source)
                    })
                })
            })
            .unwrap_or(0);
        ordered_stores.push(remaining.remove(next));
    }

    let split = ordered_stores
        .iter()
        .enumerate()
        .map(|(order, &store_index)| {
            let reloads = ordered_stores[..order]
                .iter()
                .filter_map(|&prior_index| {
                    let SIRInstruction::Store(address, offset, bits, source, _, _) =
                        &block.instructions[prior_index]
                    else {
                        return None;
                    };
                    (register_dependencies[&store_index].contains(source)
                        && store_source(store_index) != Some(*source)
                        && unit.register_map[source].width() == *bits)
                        .then(|| (*source, (*address, offset.clone(), *bits)))
                })
                .collect::<HashMap<_, _>>();
            let mut prefix = HashSet::<usize>::default();
            let mut pending = instruction_registers(&block.instructions[store_index]);
            while let Some(register) = pending.pop() {
                let Some(&definition) = definitions.get(&register) else {
                    continue;
                };
                if !prefix.insert(definition) {
                    continue;
                }
                if let Some((_, offset, _)) = reloads.get(&register) {
                    offset_registers(offset, &mut pending);
                    continue;
                }
                pending.extend(instruction_registers(&block.instructions[definition]));
            }
            let mut prefix = prefix.into_iter().collect::<Vec<_>>();
            prefix.sort_unstable();
            let mut instructions = prefix
                .into_iter()
                .map(|index| {
                    let instruction = &block.instructions[index];
                    if let Some(register) = instruction.defined_register()
                        && let Some((address, offset, bits)) = reloads.get(&register)
                    {
                        return SIRInstruction::Load(register, *address, offset.clone(), *bits);
                    }
                    instruction.clone()
                })
                .collect::<Vec<_>>();
            instructions.push(block.instructions[store_index].clone());
            let split_block = celox_sir::BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            };
            ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks: [(BlockId(0), split_block)].into_iter().collect(),
                register_map: unit.register_map.clone(),
            }
        })
        .collect::<Vec<_>>();
    let ordered_store_sites = ordered_stores
        .iter()
        .map(|&index| (unit.entry_block_id, index))
        .collect::<Vec<_>>();
    interleave_comb_runtime_effects(unit, &ordered_store_sites, split)
}

fn compile_program(
    laid_out: &LaidOutProgram,
    options: &SimulatorOptions,
    capture_trace: bool,
    cancel: Option<&CompileCancel>,
) -> Result<(NativeProgramImage, Option<NativeCodegenTrace>), SimulatorError> {
    compile_program_with_outlining_threshold(laid_out, options, capture_trace, cancel, 32_768)
}

fn compile_program_with_outlining_threshold(
    laid_out: &LaidOutProgram,
    options: &SimulatorOptions,
    capture_trace: bool,
    cancel: Option<&CompileCancel>,
    outline_threshold: usize,
) -> Result<(NativeProgramImage, Option<NativeCodegenTrace>), SimulatorError> {
    let sir = laid_out;
    let layout = laid_out.layout();
    let force_bindings =
        build_force_bindings(sir.runtime(), layout, &options.signal_access.force_signals)?;
    let (compile_tasks, task_bindings) = collect_ff_compile_tasks(sir);
    let comb_blocks = sir
        .sir
        .eval_comb
        .iter()
        .map(|unit| unit.blocks.len())
        .sum::<usize>();
    let size = |units: &[&ExecutionUnit<RegionedAbsoluteAddr>]| {
        (
            units.iter().map(|unit| unit.blocks.len()).sum::<usize>(),
            units
                .iter()
                .flat_map(|unit| unit.blocks.values())
                .map(|block| block.instructions.len())
                .sum::<usize>(),
        )
    };
    let comb_size = size(&sir.sir.eval_comb.iter().collect::<Vec<_>>());
    let task_sizes = compile_tasks
        .iter()
        .map(|task| size(&task.units))
        .collect::<Vec<_>>();
    let max_task_blocks = task_sizes.iter().map(|size| size.0).max().unwrap_or(0);
    let max_task_instructions = task_sizes.iter().map(|size| size.1).max().unwrap_or(0);
    let max_instructions = comb_size.1.max(max_task_instructions);
    let large = |size: (usize, usize)| size.0 >= 65_536 || size.1 >= 131_072;
    let release_pages = || {
        // Measure usable capacity after returning unused large-compiler pages.
        // Small single-worker builds must not trim unrelated process heaps.
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        if large(comb_size) || large((max_task_blocks, max_task_instructions)) {
            unsafe {
                libc::malloc_trim(0);
            }
        }
    };
    release_pages();
    let workers = super::compile_resources::workers_with_primary(
        comb_size,
        (max_task_blocks, max_task_instructions),
        compile_tasks.len(),
    );
    let serial = workers == 1;
    // Start expensive functions early so the final fused function does not
    // sit behind short FF entries. Stable task identities preserve image order.
    let mut task_order = (0..compile_tasks.len()).collect::<Vec<_>>();
    task_order.sort_by_key(|&id| {
        std::cmp::Reverse(
            compile_tasks[id]
                .units
                .iter()
                .map(|unit| unit.blocks.len())
                .sum::<usize>(),
        )
    });
    if options.x86_options.diagnostics.phase_timing {
        tracing::debug!(
            "[native-timing] compile_program serial={serial} workers={workers} comb_blocks={comb_blocks} max_task_blocks={max_task_blocks} max_instructions={max_instructions} ff_tasks={} settle_partitions={} settle_check={}",
            compile_tasks.len(),
            sir.runtime()
                .runtime_schema
                .settle_partitions
                .as_ref()
                .map_or(0, |s| s.partitions),
            layout.settle_check(),
        );
    }
    // Execution units already communicate through state. Keep large units
    // separate instead of rebuilding a single allocator problem, and dispatch
    // them through the same bounded pool as the FF functions.
    let split_comb = cfg!(any(
        feature = "x86_64-codegen",
        all(target_arch = "x86_64", not(feature = "arm64-codegen"))
    )) && !capture_trace
        && sir.sir.eval_comb.len() > 1
        && comb_size.1 >= outline_threshold;
    let mut comb_chunk_jits = Vec::new();
    let next_task = AtomicUsize::new(0);
    let (comb_jit, compiled_ff_codes) = if split_comb {
        let ff_count = compile_tasks.len();
        let sizes = task_sizes
            .iter()
            .copied()
            .chain(sir.sir.eval_comb.iter().map(|unit| size(&[unit])))
            .collect::<Vec<_>>();
        let mut order = (0..sizes.len()).collect::<Vec<_>>();
        order.sort_by_key(|&index| std::cmp::Reverse(sizes[index]));
        let workers = super::compile_resources::workers(
            sizes.iter().map(|s| s.0).max().unwrap_or(0),
            sizes.iter().map(|s| s.1).max().unwrap_or(0),
            sizes.len(),
        );
        if options.x86_options.diagnostics.phase_timing {
            tracing::debug!(
                "[native-timing] comb_chunks={} workers={workers}",
                sir.sir.eval_comb.len()
            );
        }
        let codes = super::compile_resources::compile_tasks(&order, workers, |index| {
            if cancelled(cancel) {
                return Err(cancelled_error());
            }
            if index < ff_count {
                let task = &compile_tasks[index];
                compile_unit_refs(
                    &task.units,
                    layout,
                    options.four_state,
                    task.label,
                    task.merge_kind,
                    task.first_ff_unit,
                    &options.x86_options,
                    false,
                    &options.optimize_options.diagnostics,
                    cancel,
                )
            } else {
                compile_unit_refs(
                    &[&sir.sir.eval_comb[index - ff_count]],
                    layout,
                    options.four_state,
                    "eval_comb_chunk",
                    NativeMergeKind::Partial,
                    None,
                    &options.x86_options,
                    false,
                    &options.optimize_options.diagnostics,
                    cancel,
                )
            }
        })
        .map_err(|_| codegen_message("native compile worker panicked"))?;
        let mut compiled = HashMap::default();
        for (index, code) in codes.into_iter().enumerate() {
            let code = code?;
            if index < ff_count {
                compiled.insert(index, code);
            } else {
                comb_chunk_jits.push(code);
            }
        }
        let first_slot = sir.runtime().runtime_schema.extern_functions.len() + 1;
        let mut builder = celox_sir::SIRBuilder::new();
        for index in 0..comb_chunk_jits.len() {
            let slot = u32::try_from(first_slot + index)
                .ok()
                .filter(|&slot| slot < 0x1000_0000)
                .ok_or_else(|| codegen_message("too many native extern functions"))?;
            builder.emit(SIRInstruction::ExternCall {
                dst: None,
                func: celox_sir::extern_abi::NATIVE_KERNEL_CALL_TAG | slot,
                args: Vec::new(),
            });
        }
        builder.seal_block(SIRTerminator::Return);
        let caller_options = options
            .x86_options
            .clone()
            .with_native_callees(comb_chunk_jits.iter().map(|code| code.required_state_size));
        let (blocks, register_map, _) = builder.drain();
        let comb = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        let root = compile_unit_refs(
            &[&comb],
            layout,
            options.four_state,
            "eval_comb",
            NativeMergeKind::Complete,
            None,
            &caller_options,
            false,
            &options.optimize_options.diagnostics,
            cancel,
        )?;
        (root, compiled)
    } else if serial {
        let comb_jit = compile_units(
            &sir.sir.eval_comb,
            layout,
            options.four_state,
            "eval_comb",
            &options.x86_options,
            capture_trace,
            &options.optimize_options.diagnostics,
            cancel,
        )?;
        let mut compiled = HashMap::default();
        let mut previous_large = large(comb_size);
        for (task_id, task) in compile_tasks.iter().enumerate() {
            if previous_large {
                release_pages();
            }
            previous_large = large(task_sizes[task_id]);
            let code = compile_unit_refs(
                &task.units,
                layout,
                options.four_state,
                task.label,
                task.merge_kind,
                task.first_ff_unit,
                &options.x86_options,
                capture_trace,
                &options.optimize_options.diagnostics,
                cancel,
            )?;
            compiled.insert(task_id, code);
        }
        (comb_jit, compiled)
    } else {
        std::thread::scope(|scope| {
            let four_state = options.four_state;
            let x86_options = &options.x86_options;
            let comb_handle = scope.spawn(move || {
                if cancelled(cancel) {
                    return Err(cancelled_error());
                }
                compile_units(
                    &sir.sir.eval_comb,
                    layout,
                    four_state,
                    "eval_comb",
                    x86_options,
                    capture_trace,
                    &options.optimize_options.diagnostics,
                    cancel,
                )
            });
            let task_worker_count = compile_tasks.len().min(workers.saturating_sub(1));
            let task_handles = (0..task_worker_count)
                .map(|_| {
                    let next_task = &next_task;
                    let compile_tasks = &compile_tasks;
                    let task_order = &task_order;
                    scope.spawn(move || {
                        let mut compiled = Vec::new();
                        loop {
                            if cancelled(cancel) {
                                return Err(cancelled_error());
                            }
                            let index = next_task.fetch_add(1, Ordering::Relaxed);
                            let Some(&task_id) = task_order.get(index) else {
                                break;
                            };
                            let task = &compile_tasks[task_id];
                            let code = compile_unit_refs(
                                &task.units,
                                layout,
                                four_state,
                                task.label,
                                task.merge_kind,
                                task.first_ff_unit,
                                x86_options,
                                capture_trace,
                                &options.optimize_options.diagnostics,
                                cancel,
                            )?;
                            compiled.push((task_id, code));
                        }
                        Ok::<_, SimulatorError>(compiled)
                    })
                })
                .collect::<Vec<_>>();

            let comb_jit = comb_handle
                .join()
                .map_err(|_| codegen_message("native eval_comb compile thread panicked"))??;
            let mut compiled_ff_codes = HashMap::default();
            for handle in task_handles {
                let compiled = handle
                    .join()
                    .map_err(|_| codegen_message("native FF compile thread panicked"))??;
                compiled_ff_codes.extend(compiled);
            }
            Ok::<_, SimulatorError>((comb_jit, compiled_ff_codes))
        })?
    };
    release_pages();
    // A foreign-interface image can request per-unit entries so force/release
    // can reapply overrides between procedural store boundaries. Ordinary
    // images do not compile or retain this duplicate combinational code.
    let force_store_boundaries =
        options.native_force_support && options.optimize_options.opt_level() == crate::OptLevel::O0;
    let comb_runtime_units = if options.native_force_support {
        sir.sir
            .eval_comb
            .iter()
            .flat_map(|unit| {
                if force_store_boundaries {
                    split_comb_execution_unit(unit)
                } else {
                    vec![unit.clone()]
                }
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let comb_unit_jits = comb_runtime_units
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            if cancelled(cancel) {
                return Err(cancelled_error());
            }
            compile_unit_refs(
                &[unit],
                layout,
                options.four_state,
                &format!("eval_comb_unit[{index}]"),
                NativeMergeKind::Complete,
                None,
                &options.x86_options,
                false,
                &options.optimize_options.diagnostics,
                cancel,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Return completed codegen's allocator pages before budgeting the process
    // workers. Small designs leave unrelated process heaps alone.
    release_pages();
    // Outlining amortizes the dominant circuit across all derived edges.
    // Keep small circuits inline, where cross-boundary optimization is cheap.
    let comb_instructions = sir
        .sir
        .eval_comb
        .iter()
        .flat_map(|unit| unit.blocks.values())
        .map(|block| block.instructions.len())
        .sum::<usize>();
    let outline_comb = cfg!(any(
        feature = "x86_64-codegen",
        all(target_arch = "x86_64", not(feature = "arm64-codegen"))
    )) && !sir.deferred_derived_edges.is_empty()
        && comb_instructions >= outline_threshold;
    let outlined_comb = if outline_comb {
        let slot = u32::try_from(sir.runtime().runtime_schema.extern_functions.len())
            .ok()
            .filter(|&slot| slot < 0x1000_0000)
            .ok_or_else(|| codegen_message("too many native extern functions"))?;
        let mut builder = celox_sir::SIRBuilder::new();
        builder.emit(SIRInstruction::ExternCall {
            dst: None,
            func: celox_sir::extern_abi::NATIVE_KERNEL_CALL_TAG | slot,
            args: Vec::new(),
        });
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        Some([ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }])
    } else {
        None
    };
    let mut process_options = options.x86_options.clone();
    if outline_comb {
        // Nested functions must not overwrite the caller's spills, saved
        // registers, scratch values, or extern call area.
        process_options = process_options.with_native_callees([comb_jit.required_state_size]);
    }
    let derived_model = (!sir.deferred_derived_edges.is_empty()).then(|| {
        celox_design::schedule::ScheduleModel::derive(&sir.design, &sir.runtime().runtime_schema)
    });
    let process_sizes = sir
        .sir
        .processes
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            sir.deferred_derived_edges.get(&index).map_or_else(
                || {
                    (
                        unit.blocks.len(),
                        unit.blocks
                            .values()
                            .map(|block| block.instructions.len())
                            .sum(),
                    )
                },
                |edge| {
                    if outline_comb {
                        // The recipe estimate includes two complete comb
                        // copies. Both now use a single call; its fixed CFG
                        // allowance already covers those call instructions.
                        (
                            edge.blocks.saturating_sub(comb_blocks.saturating_mul(2)),
                            edge.instructions
                                .saturating_sub(comb_instructions.saturating_mul(2)),
                        )
                    } else {
                        (edge.blocks, edge.instructions)
                    }
                },
            )
        })
        .collect::<Vec<_>>();
    let max_process_blocks = process_sizes.iter().map(|size| size.0).max().unwrap_or(0);
    let max_process_instructions = process_sizes.iter().map(|size| size.1).max().unwrap_or(0);
    // The resource budget already includes overlapping SIR/MIR copies.
    // Use the expanded size, rather than the tiny reserved process entry.
    let process_workers = super::compile_resources::workers(
        max_process_blocks,
        max_process_instructions,
        process_sizes.len(),
    );
    let mut process_order = (0..process_sizes.len()).collect::<Vec<_>>();
    process_order.sort_by_key(|&index| std::cmp::Reverse(process_sizes[index]));
    if options.x86_options.diagnostics.phase_timing {
        tracing::debug!(
            "[native-timing] process_compile workers={process_workers} units={} max_blocks={max_process_blocks} max_instructions={max_process_instructions}",
            process_sizes.len()
        );
    }
    let compile_process = |index| {
        let unit = &sir.sir.processes[index];
        if cancelled(cancel) {
            return Err(cancelled_error());
        }
        // Expand, compile, then drop just this edge's SIR. Retaining every
        // expansion makes memory scale with circuit size times edge count.
        let generated = sir
            .deferred_derived_edges
            .get(&index)
            .map(|edge| {
                let model = derived_model.as_ref().expect("deferred edge model");
                if let Some(comb) = &outlined_comb {
                    edge.generate_with_comb(sir, model, comb)
                } else {
                    edge.generate(sir, model)
                }
                .map_err(codegen_message)
            })
            .transpose()?;
        let unit = generated.as_ref().unwrap_or(unit);
        compile_unit_refs(
            &[unit],
            layout,
            options.four_state,
            &format!("process[{index}]"),
            NativeMergeKind::Complete,
            None,
            &process_options,
            false,
            &options.optimize_options.diagnostics,
            cancel,
        )
    };
    let process_jits =
        super::compile_resources::compile_tasks(&process_order, process_workers, compile_process)
            .map_err(|_| codegen_message("native process compile thread panicked"))?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
    let lane_kernels = super::lanes::compile_lane_kernels(sir, options, cancel)?;
    let codegen_trace = capture_trace
        .then(|| format_native_codegen_trace(&comb_jit, &compiled_ff_codes, &compile_tasks));
    let semantic_memory_size = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .expect("native semantic-memory size overflow");
    let native_memory_size = std::iter::once(comb_jit.required_state_size)
        .chain(comb_chunk_jits.iter().map(|code| code.required_state_size))
        .chain(
            compiled_ff_codes
                .values()
                .map(|compiled| compiled.required_state_size),
        )
        .chain(
            comb_unit_jits
                .iter()
                .chain(&process_jits)
                .map(|compiled| compiled.required_state_size),
        )
        .chain(
            lane_kernels
                .iter()
                .flat_map(|kernels| kernels.functions())
                .map(|compiled| compiled.required_state_size),
        )
        .fold(semantic_memory_size, usize::max);
    let required_native_features = std::iter::once(comb_jit.required_native_features)
        .chain(
            comb_chunk_jits
                .iter()
                .map(|code| code.required_native_features),
        )
        .chain(
            compiled_ff_codes
                .values()
                .map(|compiled| compiled.required_native_features),
        )
        .chain(
            comb_unit_jits
                .iter()
                .chain(&process_jits)
                .map(|compiled| compiled.required_native_features),
        )
        .chain(
            lane_kernels
                .iter()
                .flat_map(|kernels| kernels.functions())
                .map(|compiled| compiled.required_native_features),
        )
        .fold(0, |features, required| features | required);
    let mut packed_image = Vec::new();
    let mut code_entries = Vec::with_capacity(1 + compiled_ff_codes.len());
    let mut image_symbols = Vec::new();
    let comb_offset = append_native_code(
        &mut packed_image,
        &mut code_entries,
        &mut image_symbols,
        "eval_comb".into(),
        &comb_jit,
    )?;
    let mut native_kernel_offsets = if outline_comb || split_comb {
        vec![comb_offset]
    } else {
        Vec::new()
    };
    for (index, code) in comb_chunk_jits.iter().enumerate() {
        native_kernel_offsets.push(append_native_code(
            &mut packed_image,
            &mut code_entries,
            &mut image_symbols,
            format!("eval_comb_chunk[{index}]"),
            code,
        )?);
    }
    let mut comb_unit_offsets = Vec::with_capacity(comb_unit_jits.len());
    for (index, compiled) in comb_unit_jits.iter().enumerate() {
        comb_unit_offsets.push(append_native_code(
            &mut packed_image,
            &mut code_entries,
            &mut image_symbols,
            format!("eval_comb_unit[{index}]"),
            compiled,
        )?);
    }
    let mut process_offsets = Vec::with_capacity(process_jits.len());
    for (index, compiled) in process_jits.iter().enumerate() {
        process_offsets.push(append_native_code(
            &mut packed_image,
            &mut code_entries,
            &mut image_symbols,
            format!("process[{index}]"),
            compiled,
        )?);
    }
    let mut compiled_ff_keys = compiled_ff_codes.keys().copied().collect::<Vec<_>>();
    compiled_ff_keys.sort_unstable();
    let mut task_offsets = HashMap::default();
    let mut label_indices = HashMap::<&str, usize>::default();
    for &task_id in &compiled_ff_keys {
        let task = &compile_tasks[task_id];
        let index = label_indices.entry(task.label).or_default();
        let name = format!("{}[{index}]", task.label);
        *index += 1;
        let offset = append_native_code(
            &mut packed_image,
            &mut code_entries,
            &mut image_symbols,
            name,
            &compiled_ff_codes[&task_id],
        )?;
        task_offsets.insert(task_id, offset);
    }
    let lane_image = lane_kernels
        .map(|kernels| kernels.pack(&mut packed_image, &mut code_entries, &mut image_symbols))
        .transpose()?;
    // Bind semantic event identities to image-relative function offsets. The
    // precompiled runtime turns these into process-local pointers after it has
    // copied the image into executable memory.
    let (mut addr_to_id, mut id_to_addr) = crate::backend::event_ids(sir);
    let mut next_id = id_to_addr.len();
    let mut id_to_event = Vec::new();
    let mut event_map = HashMap::default();
    let mut eval_only_event_map = HashMap::default();
    let mut apply_event_map = HashMap::default();
    let compile_ff_group = |ff_map: &HashMap<
        AbsoluteAddr,
        Vec<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
    >,
                            label: &'static str,
                            event_map_out: &mut HashMap<AbsoluteAddr, NativeEventImageRef>,
                            addr_to_id: &mut HashMap<AbsoluteAddr, usize>,
                            compiled_ff_cache: &HashMap<usize, usize>,
                            comb_apply_label: Option<&'static str>,
                            next_id: &mut usize,
                            id_to_addr: &mut Vec<AbsoluteAddr>,
                            id_to_event: &mut Vec<NativeEventImageRef>|
     -> Result<(), SimulatorError> {
        for addr in ff_map.keys() {
            let canonical = sir.design.events.canonical(*addr);
            if let Some(&event) = event_map_out.get(&canonical) {
                event_map_out.insert(*addr, event);
                continue;
            }

            let task_id = task_bindings[&(label, *addr)];
            let func_offset = compiled_ff_cache[&task_id];
            let comb_apply_offset = comb_apply_label.and_then(|label| {
                let task_id = task_bindings.get(&(label, *addr))?;
                Some(compiled_ff_cache[task_id])
            });

            let (id, is_new_id) = if let Some(&id) = addr_to_id.get(&canonical) {
                (id, false)
            } else {
                let id = *next_id;
                *next_id += 1;
                addr_to_id.insert(canonical, id);
                id_to_addr.push(canonical);
                (id, true)
            };

            let event = NativeEventImageRef {
                func_offset,
                comb_apply_offset,
                addr: canonical,
                id,
            };
            event_map_out.insert(canonical, event);
            if *addr != canonical {
                event_map_out.insert(*addr, event);
            }
            if is_new_id {
                id_to_event.push(event);
            }
        }
        Ok(())
    };

    compile_ff_group(
        &sir.sir.eval_apply_ffs,
        "eval_apply_ff",
        &mut event_map,
        &mut addr_to_id,
        &task_offsets,
        Some("eval_comb_apply_ff"),
        &mut next_id,
        &mut id_to_addr,
        &mut id_to_event,
    )?;
    compile_ff_group(
        &sir.sir.eval_only_ffs,
        "eval_only_ff",
        &mut eval_only_event_map,
        &mut addr_to_id,
        &task_offsets,
        None,
        &mut next_id,
        &mut id_to_addr,
        &mut id_to_event,
    )?;
    compile_ff_group(
        &sir.sir.apply_ffs,
        "apply_ff",
        &mut apply_event_map,
        &mut addr_to_id,
        &task_offsets,
        None,
        &mut next_id,
        &mut id_to_addr,
        &mut id_to_event,
    )?;
    id_to_event = id_to_addr
        .iter()
        .map(|address| {
            event_map
                .get(address)
                .or_else(|| eval_only_event_map.get(address))
                .or_else(|| apply_event_map.get(address))
                .copied()
                .expect("every scheduled event address resolves to an event")
        })
        .collect();
    // Pre-compute 4-state initialization regions
    let mut four_state_inits = Vec::new();
    if options.four_state {
        for (addr, &offset) in &layout.offsets {
            let is_4state = layout.is_4states.get(addr).copied().unwrap_or(false);
            if is_4state {
                let allocated_size = layout.plane_size(addr);
                four_state_inits.push((offset, allocated_size));
            }
        }
        for (addr, &rel_offset) in &layout.working_offsets {
            let offset = layout.working_base_offset + rel_offset;
            let is_4state = layout.is_4states.get(addr).copied().unwrap_or(false);
            if is_4state {
                let allocated_size = layout.plane_size(addr);
                four_state_inits.push((offset, allocated_size));
            }
        }
    }

    Ok((
        NativeProgramImage {
            code: packed_image,
            code_entries,
            symbols: image_symbols,
            comb_offset,
            comb_unit_offsets,
            process_offsets,
            required_native_features,
            event_map,
            eval_only_event_map,
            apply_event_map,
            id_to_addr,
            id_to_event,
            reflection: sir.runtime().build_design_reflection(layout),
            design: sir.runtime().design.clone(),
            testbench: sir.runtime().testbench.clone(),
            runtime_schema: NativeRuntimeSchema {
                force_bindings,
                runtime_errors: sir.runtime().runtime_schema.runtime_errors.clone(),
                runtime_event_sites: sir.runtime().runtime_schema.runtime_event_sites.clone(),
                extern_functions: sir.runtime().runtime_schema.extern_functions.clone(),
                native_kernel_offsets,
                comb_observers: sir.runtime().runtime_schema.comb_observers.clone(),
                testbench_read_roots: sir.runtime().runtime_schema.testbench_read_roots.clone(),
                rtl_writes: sir.runtime().runtime_schema.rtl_writes.clone(),
                rtl_accesses: sir.runtime().rtl_accesses.iter().copied().collect(),
                persistent_comb_state: sir
                    .runtime()
                    .runtime_schema
                    .persistent_comb_state
                    .iter()
                    .copied()
                    .collect(),
                comb_writes: sir
                    .runtime()
                    .runtime_schema
                    .comb_writes
                    .iter()
                    .copied()
                    .collect(),
                dead_store_policy: sir.runtime().runtime_schema.dead_store_policy,
                live_signals: sir
                    .runtime()
                    .runtime_schema
                    .live_signals
                    .iter()
                    .copied()
                    .collect(),
                processes: sir.runtime().runtime_schema.processes.clone(),
                process_origins: sir.runtime().runtime_schema.process_origins.clone(),
                process_frames: sir.runtime().runtime_schema.process_frames.clone(),
                watches: sir.runtime().runtime_schema.watches.clone(),
                occurrence_probes: sir.runtime().runtime_schema.occurrence_probes.clone(),
                wait_sites: sir.runtime().runtime_schema.wait_sites.clone(),
                nba_sites: sir.runtime().runtime_schema.nba_sites.clone(),
                host_request_sites: sir.runtime().runtime_schema.host_request_sites.clone(),
                declaration_startup: sir.runtime().runtime_schema.declaration_startup.clone(),
                active_commits: sir.runtime().runtime_schema.active_commits.clone(),
                time_slots: sir.runtime().runtime_schema.time_slots.clone(),
                sample_shadows: sir.runtime().runtime_schema.sample_shadows.clone(),
                initial_copies: sir.runtime().runtime_schema.initial_copies.clone(),
                scheduler: sir.runtime().runtime_schema.scheduler.clone(),
                derived_edges: sir.runtime().runtime_schema.derived_edges.clone(),
            },
            layout: layout.clone(),
            native_memory_size,
            options: NativeRuntimeOptions {
                four_state: options.four_state,
                native_tick_loop: options.x86_options.native_tick_loop,
                native_force_support: options.native_force_support,
                perf_map: options.x86_options.diagnostics.perf_map,
                kernel_selection: crate::backend::lanes::kernel_selection(options),
            },
            four_state_inits,
            lanes: lane_image,
        },
        codegen_trace,
    ))
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
    #[cfg(all(
        test,
        feature = "systemverilog",
        target_arch = "x86_64",
        not(feature = "arm64-codegen")
    ))]
    pub(crate) fn compile_outlined_image_for_test(
        program: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<NativeProgramImage, SimulatorError> {
        compile_program_with_outlining_threshold(program, options, false, None, 0)
            .map(|(image, _)| image)
    }

    #[cfg(all(
        test,
        feature = "systemverilog",
        target_arch = "x86_64",
        not(feature = "arm64-codegen")
    ))]
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
        let comb_capture_enabled = vec![0; shared.layout.runtime_event_site_layouts.len().max(1)];

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
        comb_capture_enabled: Vec<u8>,
        heap: Box<celox_heap::Heap>,
    ) -> Self {
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
        let ptr = memory.as_mut_ptr() as *mut u8;
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

impl super::super::SimBackend for NativeBackend {
    type Event = NativeEventRef;

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        if self.lane_comb_kernel() {
            let compiled = Arc::clone(&self.compiled);
            let kernels = compiled
                .lane_kernels
                .as_ref()
                .expect("checked lane kernels");
            let kernel = kernels.comb.as_ref().expect("checked comb lane kernel");
            let selection = compiled.options.kernel_selection;
            return super::super::lanes::run_selected(
                self,
                |backend| &mut *backend.lane_selectors,
                super::super::lanes::LaneKernelSlot::Comb,
                selection,
                |backend| backend.run_lane_kernel(kernels.lanes, kernel),
                |backend| backend.call_func_timed(compiled.comb_func),
            );
        }
        let func = self.compiled.comb_func;
        self.call_func_timed(func)
    }

    fn eval_apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        if self.lane_event_kernel(event) {
            let compiled = Arc::clone(&self.compiled);
            let kernels = compiled
                .lane_kernels
                .as_ref()
                .expect("checked lane kernels");
            let kernel = kernels.events[event.id]
                .as_ref()
                .expect("checked event lane kernel");
            let selection = compiled.options.kernel_selection;
            return super::super::lanes::run_selected(
                self,
                |backend| &mut *backend.lane_selectors,
                super::super::lanes::LaneKernelSlot::Event(event.id),
                selection,
                |backend| backend.run_lane_kernel(kernels.lanes, kernel),
                |backend| backend.call_func_timed(event.func),
            );
        }
        self.call_func_timed(event.func)
    }

    fn eval_comb_apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        let Some(comb_apply_func) = event.comb_apply_func else {
            self.eval_comb()?;
            return self.eval_apply_ff_at(event);
        };
        // Partitioned phases compete with the fused sequential function,
        // which shares state between its combinational and FF parts.
        if self.lane_fused_kernel(event) {
            let compiled = Arc::clone(&self.compiled);
            let kernels = compiled
                .lane_kernels
                .as_ref()
                .expect("checked lane kernels");
            let kernel = kernels.fused[event.id]
                .as_ref()
                .expect("checked fused lane kernel");
            let selection = compiled.options.kernel_selection;
            return super::super::lanes::run_selected(
                self,
                |backend| &mut *backend.lane_selectors,
                super::super::lanes::LaneKernelSlot::Fused(event.id),
                selection,
                |backend| backend.run_lane_kernel(kernels.lanes, kernel),
                |backend| backend.call_func_timed(comb_apply_func),
            );
        }
        if self.lane_comb_kernel() || self.lane_event_kernel(event) {
            let selection = self.compiled.options.kernel_selection;
            return super::super::lanes::run_selected(
                self,
                |backend| &mut *backend.lane_selectors,
                super::super::lanes::LaneKernelSlot::Fused(event.id),
                selection,
                |backend| {
                    backend.eval_comb()?;
                    backend.eval_apply_ff_at(event)
                },
                |backend| backend.call_func_timed(comb_apply_func),
            );
        }
        self.call_func_timed(comb_apply_func)
    }

    fn eval_comb_apply_ff_many_at(
        &mut self,
        event: NativeEventRef,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        let fused_sequential = self
            .lane_selectors
            .decided(super::super::lanes::LaneKernelSlot::Fused(event.id))
            == Some(celox_runtime::parallel::KernelChoice::Sequential);
        let comb_apply_func = match event.comb_apply_func {
            Some(func)
                if !(self.lane_comb_kernel() || self.lane_event_kernel(event))
                    || fused_sequential =>
            {
                func
            }
            _ => {
                if count == 0 {
                    return (0, Ok(()));
                }
                return (1, self.eval_comb_apply_ff_at(event));
            }
        };
        if self.compiled.options.native_tick_loop {
            let (executed, result) = self.call_func_many_timed(comb_apply_func, count);
            if fused_sequential {
                self.lane_selectors.advance(
                    super::super::lanes::LaneKernelSlot::Fused(event.id),
                    executed,
                );
            }
            (executed, result)
        } else if count == 0 {
            (0, Ok(()))
        } else {
            if fused_sequential {
                self.lane_selectors
                    .advance(super::super::lanes::LaneKernelSlot::Fused(event.id), 1);
            }
            (1, self.call_func_timed(comb_apply_func))
        }
    }

    fn eval_only_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        self.call_func_timed(event.func)
    }

    fn apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        self.call_func_timed(event.func)
    }

    fn run_process(&mut self, index: usize) -> Result<(), SimulatorErrorCode> {
        let func = *self
            .compiled
            .process_funcs
            .get(index)
            .ok_or(SimulatorErrorCode::InternalError)?;
        self.call_func_timed(func)
    }

    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        let layout = &self.compiled.layout;
        let offset = layout.offsets.get(addr).copied().unwrap_or(0);
        let width = layout.widths.get(addr).copied().unwrap_or(0);
        let is_4state = layout.is_4states.get(addr).copied().unwrap_or(false);
        let array_layout = layout
            .unpacked_arrays
            .get(addr)
            .map(|array| SignalArrayLayout {
                element_width: array.element_width,
                element_count: array.element_count,
                element_stride: array.element_stride,
                plane_size: array.plane_size,
            });
        SignalRef {
            offset,
            width,
            is_4state,
            array_layout,
        }
    }

    fn resolve_event(&self, addr: &AbsoluteAddr) -> NativeEventRef {
        *self
            .compiled
            .event_map
            .get(addr)
            .unwrap_or_else(|| panic!("event not found for {:?}", addr))
    }

    fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<NativeEventRef> {
        self.compiled.event_map.get(addr).copied()
    }

    fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<NativeEventRef> {
        self.compiled.eval_only_event_map.get(addr).copied()
    }

    fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<NativeEventRef> {
        self.compiled.apply_event_map.get(addr).copied()
    }

    fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
        celox_runtime::backend::SimBackend::mark_vcd_signal(self, signal);
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

    fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
        celox_runtime::backend::SimBackend::mark_vcd_signal(self, signal);
        let clear_mask = self.compiled.options.four_state && signal.is_4state;
        self.write_signal_plane(signal, false, &val);
        if clear_mask {
            self.write_signal_plane(signal, true, &BigUint::from(0u8));
        }
    }

    fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint) {
        celox_runtime::backend::SimBackend::mark_vcd_signal(self, signal);
        let write_mask = self.compiled.options.four_state && signal.is_4state;
        self.write_signal_plane(signal, false, &val);
        if write_mask {
            self.write_signal_plane(signal, true, &mask);
        }
    }

    fn get(&self, signal: SignalRef) -> BigUint {
        self.read_signal_plane(signal, false)
    }

    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        let bs = get_byte_size(signal.width);
        let provided_size = std::mem::size_of::<T>();
        if signal.array_layout.is_some() {
            let mut val = T::default();
            let value = self.read_signal_plane(signal, false).to_bytes_le();
            let val_bytes = unsafe {
                std::slice::from_raw_parts_mut(&mut val as *mut T as *mut u8, provided_size)
            };
            let copy_len = value.len().min(val_bytes.len());
            val_bytes[..copy_len].copy_from_slice(&value[..copy_len]);
            return val;
        }
        let ptr = unsafe { (self.memory.as_ptr() as *const u8).add(signal.offset) };
        if provided_size <= bs {
            return unsafe { std::ptr::read_unaligned(ptr as *const T) };
        }

        let bytes = self.mem_bytes();
        let mut val = T::default();
        let val_bytes =
            unsafe { std::slice::from_raw_parts_mut(&mut val as *mut T as *mut u8, provided_size) };
        let copy_len = val_bytes.len().min(bs);
        val_bytes[..copy_len].copy_from_slice(&bytes[signal.offset..signal.offset + copy_len]);
        val
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        let val = self.read_signal_plane(signal, false);
        let mask = if self.compiled.options.four_state && signal.is_4state {
            self.read_signal_plane(signal, true)
        } else {
            BigUint::from(0u32)
        };
        (val, mask)
    }

    fn memory_as_ptr(&self) -> (*const u8, usize) {
        (self.mem_ptr(), self.memory.len_words() * 8)
    }

    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        let view = (
            self.memory.expose_mut_ptr().cast(),
            self.memory.len_words() * 8,
        );
        self.mark_settle_untracked();
        view
    }

    fn memory_owner(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        Some(self.memory.owner())
    }

    fn write_memory(&mut self, offset: usize, bytes: &[u8]) {
        self.memory.write_bytes(offset, bytes);
        self.mark_settle_range(offset, bytes.len());
        if !self.memory.vcd_tracking_enabled() {
            self.mark_settle_untracked();
        }
    }

    fn vcd_tracking_enabled(&self) -> bool {
        self.memory.vcd_tracking_enabled()
    }

    fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize) {
        (
            self.runtime_event_buffer.as_ptr(),
            self.runtime_event_buffer.byte_size(),
        )
    }

    fn runtime_event_buffer(&self) -> Option<Arc<RuntimeEventBuffer>> {
        Some(Arc::clone(&self.runtime_event_buffer))
    }

    fn set_comb_capture_event_enabled(&mut self, active_sites: &[bool]) {
        self.comb_capture_enabled.fill(0);
        for (idx, active) in active_sites.iter().copied().enumerate() {
            if active && idx < self.comb_capture_enabled.len() {
                self.comb_capture_enabled[idx] = 1;
            }
        }
    }

    fn comb_capture_event_enabled(&self) -> Vec<bool> {
        self.comb_capture_enabled
            .iter()
            .map(|&enabled| enabled != 0)
            .collect()
    }

    fn stable_region_size(&self) -> usize {
        self.compiled.layout.total_size
    }

    fn layout(&self) -> &MemoryLayout {
        &self.compiled.layout
    }

    fn heap(&self) -> &celox_heap::Heap {
        &self.heap
    }

    fn heap_mut(&mut self) -> &mut celox_heap::Heap {
        &mut self.heap
    }

    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        &self.compiled.id_to_addr
    }

    fn id_to_event_slice(&self) -> &[NativeEventRef] {
        &self.compiled.id_to_event
    }

    fn num_events(&self) -> usize {
        self.compiled.id_to_event.len()
    }

    fn clear_triggered_bits(&mut self) {
        let offset = self.compiled.layout.triggered_bits_offset;
        let size = self.compiled.layout.triggered_bits_total_size;
        let bytes = self.mem_bytes_mut();
        bytes[offset..offset + size].fill(0);
    }

    fn mark_triggered_bit(&mut self, id: usize) {
        let offset = self.compiled.layout.triggered_bits_offset;
        let byte_idx = offset + id / 8;
        let bit_idx = id % 8;
        self.mem_bytes_mut()[byte_idx] |= 1 << bit_idx;
    }

    fn get_triggered_bits(&self) -> BitSet {
        let offset = self.compiled.layout.triggered_bits_offset;
        let size = self.compiled.layout.triggered_bits_total_size;
        celox_runtime::backend::triggered_bits_from_bytes(&self.mem_bytes()[offset..offset + size])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        BasicBlock, BlockId, RegisterId, RegisterType, SIRInstruction, SIRTerminator, SIRValue,
    };

    /// A unit of `blocks` blocks loading `value`, with its maps filled in
    /// forward or reverse order.
    fn unit(blocks: usize, value: u8, width: usize, reverse: bool) -> NativeUnit {
        let mut ids = (0..blocks).collect::<Vec<_>>();
        if reverse {
            ids.reverse();
        }
        NativeUnit {
            entry_block_id: BlockId(0),
            blocks: ids
                .iter()
                .map(|&id| {
                    let terminator = if id + 1 < blocks {
                        SIRTerminator::Jump(BlockId(id + 1), vec![])
                    } else {
                        SIRTerminator::Return
                    };
                    let block = BasicBlock {
                        id: BlockId(id),
                        params: vec![],
                        instructions: vec![SIRInstruction::Imm(
                            RegisterId(id),
                            SIRValue::new(value),
                        )],
                        terminator,
                    };
                    (BlockId(id), block)
                })
                .collect(),
            register_map: ids
                .iter()
                .map(|&id| (RegisterId(id), RegisterType::Logic { width }))
                .collect(),
        }
    }

    #[test]
    fn unit_classes_match_deep_comparison() {
        let mut pool = Vec::new();
        for blocks in 1..4 {
            for value in 0..2 {
                for width in [1, 8] {
                    for reverse in [false, true] {
                        pool.push(unit(blocks, value, width, reverse));
                    }
                }
            }
        }
        let units = pool.iter().collect::<Vec<_>>();
        // Lists of up to two units, repeating the same references as the
        // shared prefix of comb-apply tasks does.
        let lists = std::iter::once(Vec::new())
            .chain(units.iter().map(|unit| vec![*unit]))
            .chain(
                units
                    .iter()
                    .flat_map(|a| units.iter().map(move |b| vec![*a, *b])),
            )
            .collect::<Vec<_>>();
        let mut classes = NativeUnitClasses::default();
        let keys = lists
            .iter()
            .map(|list| classes.classes(list))
            .collect::<Vec<_>>();
        for (a, key_a) in lists.iter().zip(&keys) {
            for (b, key_b) in lists.iter().zip(&keys) {
                assert_eq!(a == b, key_a == key_b, "{a:?} {b:?}");
            }
        }
        // The pool has equal units with differently ordered maps.
        assert!(classes.count < pool.len());
    }
}
