//! NativeBackend: SimBackend implementation using a custom host backend.
//!
//! Mirrors the structure of JitBackend but compiles through
//! ISel → scalar MIR → regalloc → host emission instead of Cranelift.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bit_set::BitSet;
use celox_design::{
    ElaboratedDesign, EventTopology, InitialStateData, InitialStateValue, InitialStateWriteRun,
    RuntimeCombObserver, RuntimeErrorInfo, RuntimeEventSite, RuntimeSchema,
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
use super::super::traits::SimulatorErrorCode;
use super::super::{MemoryLayout, get_byte_size};
use super::{emit, jit_mem, regalloc};

const NATIVE_FEATURE_BMI2: u8 = 1 << 0;
const NATIVE_FEATURE_AVX: u8 = 1 << 1;
const NATIVE_FEATURE_FS_STATE_BASE: u8 = 1 << 2;
const NATIVE_FEATURE_GS_STATE_BASE: u8 = 1 << 3;
const NATIVE_FEATURE_POPCNT: u8 = 1 << 4;
const KNOWN_NATIVE_FEATURES: u8 = NATIVE_FEATURE_BMI2
    | NATIVE_FEATURE_AVX
    | NATIVE_FEATURE_FS_STATE_BASE
    | NATIVE_FEATURE_GS_STATE_BASE
    | NATIVE_FEATURE_POPCNT;

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
    pub comb_apply_func: NativeSimFunc,
    pub addr: AbsoluteAddr,
    pub id: usize,
}

impl std::fmt::Debug for NativeEventRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeEventRef")
            .field("func", &(self.func as usize))
            .field("comb_apply_func", &(self.comb_apply_func as usize))
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
    native_memory_size: usize,
    options: NativeRuntimeOptions,
    /// (offset, byte_size) pairs for 4-state variables that need X initialization.
    four_state_inits: Vec<(usize, usize)>,
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
        let materialize = |event: NativeEventImageRef| -> Result<NativeEventRef, SimulatorError> {
            Ok(NativeEventRef {
                func: native_function_at(&jit_image, event.func_offset)?,
                comb_apply_func: native_function_at(&jit_image, event.comb_apply_offset)?,
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
        let event_map = materialize_map(&program_image.event_map)?;
        let eval_only_event_map = materialize_map(&program_image.eval_only_event_map)?;
        let apply_event_map = materialize_map(&program_image.apply_event_map)?;
        let id_to_event = program_image
            .id_to_event
            .iter()
            .copied()
            .map(materialize)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            comb_func,
            comb_unit_funcs,
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
    comb_apply_offset: usize,
    addr: AbsoluteAddr,
    id: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct NativeCodeSymbol {
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NativeRuntimeSchema {
    pub(crate) runtime_errors: HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
    pub(crate) runtime_event_sites: Vec<RuntimeEventSite>,
    pub(crate) comb_observers: Vec<RuntimeCombObserver<AbsoluteAddr>>,
    pub(crate) testbench_read_roots: HashSet<AbsoluteAddr>,
    pub(crate) rtl_writes: HashSet<celox_design::VarAtomBase<AbsoluteAddr>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NativeEventTopology {
    aliases: HashMap<AbsoluteAddr, AbsoluteAddr>,
    ordered_events: Vec<AbsoluteAddr>,
    cascaded_events: std::collections::BTreeSet<AbsoluteAddr>,
    reset_clocks: HashMap<AbsoluteAddr, AbsoluteAddr>,
}

impl NativeEventTopology {
    pub(crate) fn canonical(&self, address: AbsoluteAddr) -> AbsoluteAddr {
        self.aliases.get(&address).copied().unwrap_or(address)
    }
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
    required_native_features: u8,
    event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    eval_only_event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    apply_event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    id_to_addr: Vec<AbsoluteAddr>,
    id_to_event: Vec<NativeEventImageRef>,
    reflection: DesignReflection,
    frontend: crate::ir::FrontendLookup,
    initial_state: Vec<InitialStateValue<AbsoluteAddr>>,
    testbench: Option<TestbenchProgram<AbsoluteAddr>>,
    runtime_schema: NativeRuntimeSchema,
    event_topology: NativeEventTopology,
    layout: MemoryLayout,
    native_memory_size: usize,
    options: NativeRuntimeOptions,
    four_state_inits: Vec<(usize, usize)>,
}

impl NativeProgramImage {
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

    /// Source-independent runtime diagnostics and combinational observers.
    pub(crate) fn runtime_schema(&self) -> &NativeRuntimeSchema {
        &self.runtime_schema
    }

    /// Whether the image's generated code and state layout use four-state data.
    pub(crate) fn four_state(&self) -> bool {
        self.options.four_state
    }

    /// Canonical event-domain topology used by the runtime scheduler.
    pub(crate) fn event_topology(&self) -> &NativeEventTopology {
        &self.event_topology
    }

    /// Reconstruct the source-independent runtime metadata retained by this
    /// image. No frontend parsing or SIR/layout work is needed on the
    /// execution side of a host-codegen workflow.
    pub(crate) fn runtime_program(&self) -> crate::ir::RuntimeProgram {
        crate::ir::RuntimeProgram {
            design: ElaboratedDesign {
                state_objects: HashMap::default(),
                events: EventTopology {
                    aliases: self.event_topology.aliases.clone(),
                    ordered_events: self.event_topology.ordered_events.clone(),
                    cascaded_events: self.event_topology.cascaded_events.clone(),
                    reset_clocks: self.event_topology.reset_clocks.clone(),
                },
                initial_state: self.initial_state.clone(),
            },
            frontend: self.frontend.clone(),
            runtime_schema: RuntimeSchema {
                runtime_errors: self.runtime_schema.runtime_errors.clone(),
                runtime_event_sites: self.runtime_schema.runtime_event_sites.clone(),
                comb_observers: self.runtime_schema.comb_observers.clone(),
                testbench_read_roots: self.runtime_schema.testbench_read_roots.clone(),
                rtl_writes: self.runtime_schema.rtl_writes.clone(),
            },
            testbench: self.testbench.clone(),
        }
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        self.reflection
            .validate()
            .map_err(|error| format!("invalid design reflection: {error}"))?;
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
        if self
            .comb_unit_offsets
            .iter()
            .any(|offset| !entry_offsets.contains(offset))
        {
            return Err("a combinational unit offset does not name an image entry".into());
        }
        if self.required_native_features & !KNOWN_NATIVE_FEATURES != 0 {
            return Err("native image contains unknown feature requirements".into());
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
                || !entry_offsets.contains(&event.comb_apply_offset)
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

fn codegen_err(error: CodegenError) -> SimulatorError {
    error.into()
}

fn codegen_message(message: impl Into<String>) -> SimulatorError {
    codegen_err(CodegenError::message(message))
}

struct CompiledNativeFunction {
    code: Vec<u8>,
    symbols: Vec<jit_mem::JitSymbol>,
    trace: Option<emit::NativeFunctionTrace>,
    required_state_size: usize,
    required_native_features: u8,
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

fn prepare_merged_sir(
    units: &[&crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    label: &str,
    first_ff_unit: Option<usize>,
    diagnostics: &crate::optimizer::SirDiagnostics,
) -> Result<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>, SimulatorError> {
    let verify_enabled = cfg!(debug_assertions) || diagnostics.verify_boundaries;
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
    if label == "eval_comb_apply_ff"
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
    crate::optimizer::sir::pass_eliminate_working_round_trip::eliminate_working_round_trip(
        &mut sir_eu,
        &boundaries,
    );
    verify(&sir_eu, "after x86 direct working rewrite")?;
    let promoted_working =
        crate::optimizer::sir::promote_eval_apply_working_round_trips(&mut sir_eu);
    if promoted_working {
        verify(&sir_eu, "after x86 working StateSSA")?;
        crate::optimizer::sir::remove_dead_sir_definitions(&mut sir_eu);
        verify(&sir_eu, "after x86 working StateSSA DCE")?;
    }
    crate::optimizer::sir::optimize_native_merged_chain(
        &mut sir_eu,
        layout,
        four_state,
        label == "eval_comb_apply_ff",
        diagnostics,
    )
    .map_err(|source| {
        codegen_err(CodegenError::Optimization {
            context: "native merged-chain optimization",
            source,
        })
    })?;
    verify(&sir_eu, "after x86 merged-chain cleanup")?;
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
) -> Result<CompiledNativeFunction, SimulatorError> {
    let units = units.iter().collect::<Vec<_>>();
    compile_unit_refs(
        &units,
        layout,
        four_state,
        label,
        None,
        x86_options,
        capture_trace,
        diagnostics,
    )
}

fn compile_unit_refs(
    units: &[&crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    label: &str,
    first_ff_unit: Option<usize>,
    x86_options: &crate::backend::X86BackendOptions,
    capture_trace: bool,
    diagnostics: &crate::optimizer::SirDiagnostics,
) -> Result<CompiledNativeFunction, SimulatorError> {
    let timing = x86_options.diagnostics.phase_timing;
    if units.is_empty() {
        // Empty function: just return 0
        let mut empty_func = super::mir::MFunction::new(super::mir::VRegAllocator::new(), vec![]);
        let mut block = super::mir::MBlock::new(super::mir::BlockId(0));
        block.push(super::mir::MInst::Return);
        empty_func.push_block(block);
        let empty_result = emit::emit(&empty_func, &regalloc::AssignmentMap::default(), 0)
            .map_err(|source| codegen_err(CodegenError::NativeEmission { source }))?;
        let trace = capture_trace.then(|| emit::NativeFunctionTrace {
            optimized_sir: "<empty native function>\n".into(),
            reactive_graph: String::new(),
            state_layout: String::new(),
            mir_before_regalloc: empty_func.to_string(),
            mir_after_late_memory_folds: empty_func.to_string(),
            mir_after_scheduling: empty_func.to_string(),
            mir_after_regalloc: empty_func.to_string(),
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
    let sir_eu = prepare_merged_sir(units, layout, four_state, label, first_ff_unit, diagnostics)?;
    let mut trace = capture_trace.then(emit::NativeFunctionTrace::default);
    let emit_result = emit::emit_prepared_eu(
        &sir_eu,
        layout,
        four_state,
        label,
        x86_options,
        trace.as_mut(),
    )
    .map_err(|source| codegen_err(CodegenError::NativePipeline { source }))?;
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

fn append_native_code(
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

fn native_function_at(
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
    first_ff_unit: Option<usize>,
    bindings: Vec<String>,
}

type NativeTaskBindings = HashMap<(&'static str, AbsoluteAddr), usize>;

fn collect_ff_compile_tasks(
    sir: &LaidOutProgram,
) -> (Vec<NativeCompileTask<'_>>, NativeTaskBindings) {
    let mut tasks = Vec::new();
    let mut task_bindings = HashMap::default();
    collect_ff_compile_tasks_from(
        sir,
        &sir.sir.eval_apply_ffs,
        "eval_apply_ff",
        &mut tasks,
        &mut task_bindings,
    );
    collect_ff_compile_tasks_from(
        sir,
        &sir.sir.eval_only_ffs,
        "eval_only_ff",
        &mut tasks,
        &mut task_bindings,
    );
    collect_ff_compile_tasks_from(
        sir,
        &sir.sir.apply_ffs,
        "apply_ff",
        &mut tasks,
        &mut task_bindings,
    );
    collect_comb_apply_compile_tasks(sir, &mut tasks, &mut task_bindings);
    (tasks, task_bindings)
}

fn collect_comb_apply_compile_tasks<'a>(
    sir: &'a LaidOutProgram,
    tasks: &mut Vec<NativeCompileTask<'a>>,
    task_bindings: &mut NativeTaskBindings,
) {
    const LABEL: &str = "eval_comb_apply_ff";
    for (addr, ff_units) in &sir.sir.eval_apply_ffs {
        let fused_units = sir.sir.eval_comb_apply_ffs.get(addr);
        let (unit_refs, first_ff_unit) = if let Some(fused_units) = fused_units {
            (fused_units.iter().collect::<Vec<_>>(), None)
        } else {
            let mut unit_refs = sir.sir.eval_comb.iter().collect::<Vec<_>>();
            let first_ff_unit =
                (!unit_refs.is_empty() && !ff_units.is_empty()).then_some(unit_refs.len());
            unit_refs.extend(ff_units);
            (unit_refs, first_ff_unit)
        };
        let binding = format!("{LABEL} trigger={}", sir.get_path(addr));
        let index = if let Some(index) = tasks.iter().position(|task| {
            task.label == LABEL && task.first_ff_unit == first_ff_unit && task.units == unit_refs
        }) {
            tasks[index].bindings.push(binding);
            index
        } else {
            let index = tasks.len();
            tasks.push(NativeCompileTask {
                units: unit_refs,
                label: LABEL,
                first_ff_unit,
                bindings: vec![binding],
            });
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
    tasks: &mut Vec<NativeCompileTask<'a>>,
    task_bindings: &mut NativeTaskBindings,
) {
    for (addr, units) in ff_map {
        let unit_refs = units.iter().collect::<Vec<_>>();
        let binding = format!("{label} trigger={}", sir.get_path(addr));
        let index = if let Some(index) = tasks.iter().position(|task| task.units == unit_refs) {
            tasks[index].bindings.push(binding);
            index
        } else {
            let index = tasks.len();
            tasks.push(NativeCompileTask {
                units: unit_refs,
                label,
                first_ff_unit: None,
                bindings: vec![binding],
            });
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
    mir.push_str("Native disassembly of emitted function:\n");
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
    match instruction {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => registers.extend([*lhs, *rhs]),
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            registers.push(*source);
        }
        SIRInstruction::Load(_, _, offset, _) => offset_registers(offset, &mut registers),
        SIRInstruction::Store(_, offset, _, source, _, _) => {
            registers.push(*source);
            offset_registers(offset, &mut registers);
        }
        SIRInstruction::Commit(..) => {}
        SIRInstruction::Concat(_, sources) => registers.extend(sources),
        SIRInstruction::Mux(_, condition, then_value, else_value) => {
            registers.extend([*condition, *then_value, *else_value]);
        }
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => registers.extend(args),
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            registers.extend([*old, *new]);
        }
    }
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
    matches!(
        instruction,
        SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. }
    )
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
                                SIRInstruction::Store(..) | SIRInstruction::Commit(..) => {
                                    (site == target).then_some(instruction)
                                }
                                SIRInstruction::RuntimeEvent { .. }
                                | SIRInstruction::CombCaptureEvent { .. }
                                | SIRInstruction::CombCaptureEnableIfChanged { .. } => None,
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
) -> Result<(NativeProgramImage, Option<NativeCodegenTrace>), SimulatorError> {
    const MAX_PARALLEL_NATIVE_FUNCTIONS: usize = 4;

    let sir = laid_out;
    let layout = laid_out.layout();
    let (compile_tasks, task_bindings) = collect_ff_compile_tasks(sir);
    let next_task = AtomicUsize::new(0);
    let (comb_jit, compiled_ff_codes) = std::thread::scope(|scope| {
        let four_state = options.four_state;
        let x86_options = &options.x86_options;
        let comb_handle = scope.spawn(move || {
            compile_units(
                &sir.sir.eval_comb,
                layout,
                four_state,
                "eval_comb",
                x86_options,
                capture_trace,
                &options.optimize_options.diagnostics,
            )
        });
        let task_worker_count = compile_tasks
            .len()
            .min(MAX_PARALLEL_NATIVE_FUNCTIONS.saturating_sub(1));
        let task_handles = (0..task_worker_count)
            .map(|_| {
                let next_task = &next_task;
                let compile_tasks = &compile_tasks;
                scope.spawn(move || {
                    let mut compiled = Vec::new();
                    loop {
                        let task_id = next_task.fetch_add(1, Ordering::Relaxed);
                        let Some(task) = compile_tasks.get(task_id) else {
                            break;
                        };
                        let code = compile_unit_refs(
                            &task.units,
                            layout,
                            four_state,
                            task.label,
                            task.first_ff_unit,
                            x86_options,
                            capture_trace,
                            &options.optimize_options.diagnostics,
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
    })?;
    // A foreign-interface image can request per-unit entries so force/release
    // can reapply overrides between procedural store boundaries. Ordinary
    // images do not compile or retain this duplicate combinational code.
    let force_store_boundaries = options.optimize_options.opt_level() == crate::OptLevel::O0;
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
            compile_unit_refs(
                &[unit],
                layout,
                options.four_state,
                &format!("eval_comb_unit[{index}]"),
                None,
                &options.x86_options,
                false,
                &options.optimize_options.diagnostics,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let codegen_trace = capture_trace
        .then(|| format_native_codegen_trace(&comb_jit, &compiled_ff_codes, &compile_tasks));
    let semantic_memory_size = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .expect("native semantic-memory size overflow");
    let native_memory_size = std::iter::once(comb_jit.required_state_size)
        .chain(
            compiled_ff_codes
                .values()
                .map(|compiled| compiled.required_state_size),
        )
        .chain(
            comb_unit_jits
                .iter()
                .map(|compiled| compiled.required_state_size),
        )
        .fold(semantic_memory_size, usize::max);
    let required_native_features = std::iter::once(comb_jit.required_native_features)
        .chain(
            compiled_ff_codes
                .values()
                .map(|compiled| compiled.required_native_features),
        )
        .chain(
            comb_unit_jits
                .iter()
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
    // Bind semantic event identities to image-relative function offsets. The
    // precompiled runtime turns these into process-local pointers after it has
    // copied the image into executable memory.
    let mut next_id = 0usize;
    let mut id_to_addr = Vec::new();
    let mut id_to_event = Vec::new();
    let mut event_map = HashMap::default();
    let mut eval_only_event_map = HashMap::default();
    let mut apply_event_map = HashMap::default();
    let mut addr_to_id = HashMap::default();
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
            let comb_apply_offset = comb_apply_label
                .map(|label| {
                    let task_id = task_bindings[&(label, *addr)];
                    compiled_ff_cache[&task_id]
                })
                .unwrap_or(func_offset);

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
            required_native_features,
            event_map,
            eval_only_event_map,
            apply_event_map,
            id_to_addr,
            id_to_event,
            reflection: sir.runtime().build_design_reflection(layout),
            frontend: sir.runtime().frontend.clone(),
            initial_state: sir.runtime().design.initial_state.clone(),
            testbench: sir.runtime().testbench.clone(),
            runtime_schema: NativeRuntimeSchema {
                runtime_errors: sir.runtime().runtime_schema.runtime_errors.clone(),
                runtime_event_sites: sir.runtime().runtime_schema.runtime_event_sites.clone(),
                comb_observers: sir.runtime().runtime_schema.comb_observers.clone(),
                testbench_read_roots: sir.runtime().runtime_schema.testbench_read_roots.clone(),
                rtl_writes: sir.runtime().runtime_schema.rtl_writes.clone(),
            },
            event_topology: NativeEventTopology {
                aliases: sir.runtime().design.events.aliases.clone(),
                ordered_events: sir.runtime().design.events.ordered_events.clone(),
                cascaded_events: sir.runtime().design.events.cascaded_events.clone(),
                reset_clocks: sir.runtime().design.events.reset_clocks.clone(),
            },
            layout: layout.clone(),
            native_memory_size,
            options: NativeRuntimeOptions {
                four_state: options.four_state,
                native_tick_loop: options.x86_options.native_tick_loop,
                native_force_support: options.native_force_support,
                perf_map: options.x86_options.diagnostics.perf_map,
            },
            four_state_inits,
        },
        codegen_trace,
    ))
}

// ────────────────────────────────────────────────────────────────
// NativeBackend
// ────────────────────────────────────────────────────────────────

pub struct NativeBackend {
    compiled: Arc<SharedNativeCode>,
    memory: Vec<u64>,
    runtime_event_buffer: Arc<RuntimeEventBuffer>,
    comb_capture_enabled: Vec<u8>,
    execution_timing: Option<NativeExecutionTiming>,
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
        let (image, trace) = compile_program(laid_out, options, false)?;
        debug_assert!(trace.is_none());
        Ok(image)
    }

    pub(crate) fn compile_image_with_codegen_trace(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<(NativeProgramImage, NativeCodegenTrace), SimulatorError> {
        let (image, trace) = compile_program(laid_out, options, true)?;
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
        let shared = Arc::new(unsafe { SharedNativeCode::from_image(image)? });
        Ok(Self::from_shared(shared))
    }

    pub fn new(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<Self, SimulatorError> {
        let image = Self::compile_image(laid_out, options)?;
        // Safety: `image` was produced in-process by the Celox compiler above.
        unsafe { Self::from_image(image) }
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
        let shared = unsafe { SharedNativeCode::from_image(image)? };
        let backend = Self::from_shared(Arc::new(shared));
        Ok((backend, trace))
    }

    /// Create a new backend instance from shared compiled code.
    /// Each instance gets its own simulation state memory.
    pub fn from_shared(shared: Arc<SharedNativeCode>) -> Self {
        let mem_size_words = shared.native_memory_size.div_ceil(8);
        let mut memory = vec![0u64; mem_size_words + 1]; // +1 for safety
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

        let mut backend = Self {
            compiled: shared,
            memory,
            runtime_event_buffer,
            comb_capture_enabled,
            execution_timing: None,
        };
        backend.install_event_buffers();
        let compiled = Arc::clone(&backend.compiled);
        backend.apply_initial_values(&compiled.program_image.initial_state);
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
    }

    /// Stop timing and return the accumulated generated-code interval.
    pub fn finish_execution_timing(&mut self) -> Option<NativeExecutionTiming> {
        self.execution_timing.take()
    }

    fn install_event_buffers(&mut self) {
        use crate::backend::memory_layout::{
            STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET, STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
        };

        let addr = self.runtime_event_buffer.as_mut_ptr() as u64;
        let ptr = unsafe {
            (self.memory.as_mut_ptr() as *mut u8).add(STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET)
                as *mut u64
        };
        unsafe {
            std::ptr::write_unaligned(ptr, addr);
        }
        let addr = self.comb_capture_enabled.as_ptr() as u64;
        let ptr = unsafe {
            (self.memory.as_mut_ptr() as *mut u8).add(STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET)
                as *mut u64
        };
        unsafe {
            std::ptr::write_unaligned(ptr, addr);
        }
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
        let len = self.memory.len() * 8;
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }

    fn mem_bytes_mut(&mut self) -> &mut [u8] {
        let ptr = self.mem_mut_ptr();
        let len = self.memory.len() * 8;
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
        let element_bytes = get_byte_size(array.element_width);
        let element_mask = (BigUint::from(1u8) << array.element_width) - BigUint::from(1u8);
        let mut value = BigUint::from(0u8);
        for element in 0..array.element_count {
            let start = plane_offset + element * array.element_stride;
            let element_value =
                BigUint::from_bytes_le(&bytes[start..start + element_bytes]) & &element_mask;
            value |= element_value << (element * array.element_width);
        }
        value
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
        let element_bytes = get_byte_size(array.element_width);
        let element_mask = (BigUint::from(1u8) << array.element_width) - BigUint::from(1u8);
        let bytes = self.mem_bytes_mut();
        bytes[plane_offset..plane_offset + array.plane_size].fill(0);
        for element in 0..array.element_count {
            let element_value = (value >> (element * array.element_width)) & &element_mask;
            let value_bytes = element_value.to_bytes_le();
            let copy_len = value_bytes.len().min(element_bytes);
            let start = plane_offset + element * array.element_stride;
            bytes[start..start + copy_len].copy_from_slice(&value_bytes[..copy_len]);
        }
    }

    fn call_func(memory: &mut [u64], func: NativeSimFunc) -> Result<(), SimulatorErrorCode> {
        let ptr = memory.as_mut_ptr() as *mut u8;
        let ret = unsafe { func(ptr) };
        match ret {
            0 => Ok(()),
            code if code > 0 => Err(SimulatorErrorCode::DetectedTrueLoopCode(code)),
            _ => Err(SimulatorErrorCode::InternalError),
        }
    }

    fn call_func_timed(&mut self, func: NativeSimFunc) -> Result<(), SimulatorErrorCode> {
        let Some(_) = self.execution_timing else {
            return Self::call_func(&mut self.memory, func);
        };
        let start = Instant::now();
        let result = Self::call_func(&mut self.memory, func);
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
            code if code > 0 => Err(SimulatorErrorCode::DetectedTrueLoopCode(code)),
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
            return Self::call_func_many(&mut self.memory, func, count);
        }
        let start = Instant::now();
        let result = Self::call_func_many(&mut self.memory, func, count);
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
        let func = self.compiled.comb_func;
        self.call_func_timed(func)
    }

    fn eval_apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        self.call_func_timed(event.func)
    }

    fn eval_comb_apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        self.call_func_timed(event.comb_apply_func)
    }

    fn eval_comb_apply_ff_many_at(
        &mut self,
        event: NativeEventRef,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        if self.compiled.options.native_tick_loop {
            self.call_func_many_timed(event.comb_apply_func, count)
        } else if count == 0 {
            (0, Ok(()))
        } else {
            (1, self.call_func_timed(event.comb_apply_func))
        }
    }

    fn eval_only_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        self.call_func_timed(event.func)
    }

    fn apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        self.call_func_timed(event.func)
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
        let allocated_size = get_byte_size(signal.width);
        let provided_size = std::mem::size_of::<T>();
        let clear_mask = self.compiled.options.four_state && signal.is_4state;

        assert!(provided_size <= allocated_size);

        if signal.array_layout.is_some() {
            let value_bytes =
                unsafe { std::slice::from_raw_parts(&val as *const T as *const u8, provided_size) };
            self.write_signal_plane(signal, false, &BigUint::from_bytes_le(value_bytes));
            if clear_mask {
                self.write_signal_plane(signal, true, &BigUint::from(0u8));
            }
            return;
        }

        unsafe {
            let base_ptr = (self.memory.as_mut_ptr() as *mut u8).add(signal.offset);
            if !clear_mask && allocated_size == 1 {
                let raw = *(&val as *const T as *const u8);
                let byte = if signal.width < 8 {
                    raw & ((1u8 << signal.width) - 1)
                } else {
                    raw
                };
                *base_ptr = byte;
                return;
            }

            if provided_size < allocated_size {
                std::ptr::write_bytes(base_ptr, 0, allocated_size);
            }
            std::ptr::write_unaligned(base_ptr as *mut T, val);

            if clear_mask {
                let mask_ptr = base_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0, allocated_size);
            }
        }
    }

    fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
        let clear_mask = self.compiled.options.four_state && signal.is_4state;
        self.write_signal_plane(signal, false, &val);
        if clear_mask {
            self.write_signal_plane(signal, true, &BigUint::from(0u8));
        }
    }

    fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint) {
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
        (self.mem_ptr(), self.memory.len() * 8)
    }

    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        (self.mem_mut_ptr(), self.memory.len() * 8)
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

    fn stable_region_size(&self) -> usize {
        self.compiled.layout.total_size
    }

    fn layout(&self) -> &MemoryLayout {
        &self.compiled.layout
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
        let bytes = self.mem_bytes();
        let mut bs = BitSet::with_capacity(size * 8);
        for i in 0..size * 8 {
            let byte_idx = offset + i / 8;
            let bit_idx = i % 8;
            if bytes[byte_idx] & (1 << bit_idx) != 0 {
                bs.insert(i);
            }
        }
        bs
    }
}
