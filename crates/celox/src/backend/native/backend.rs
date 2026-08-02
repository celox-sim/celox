//! NativeBackend: SimBackend implementation using the custom x86-64 backend.
//!
//! Mirrors the structure of JitBackend but compiles through
//! ISel → MIR → regalloc → x86-64 emit instead of Cranelift.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bit_set::BitSet;
use num_bigint::BigUint;

use crate::ir::{AbsoluteAddr, LaidOutProgram, SignalArrayLayout, SignalRef};
use crate::{CodegenError, HashMap, SimulatorError, SimulatorOptions};

use super::super::RuntimeEventBuffer;
use super::super::traits::SimulatorErrorCode;
use super::super::{MemoryLayout, get_byte_size};
use super::{emit, jit_mem, regalloc};

// ────────────────────────────────────────────────────────────────
// Event handle
// ────────────────────────────────────────────────────────────────

/// JIT function type: `fn(state: *mut u8) -> i64`
pub type NativeSimFunc = unsafe extern "sysv64" fn(*mut u8) -> i64;

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
    /// Keep JitCode alive so the mmap regions remain valid.
    _jit_codes: Vec<jit_mem::JitCode>,

    event_map: HashMap<AbsoluteAddr, NativeEventRef>,
    eval_only_event_map: HashMap<AbsoluteAddr, NativeEventRef>,
    apply_event_map: HashMap<AbsoluteAddr, NativeEventRef>,
    id_to_addr: Vec<AbsoluteAddr>,
    id_to_event: Vec<NativeEventRef>,
    layout: MemoryLayout,
    /// Simulation-state bytes plus the largest native spill/scratch arena
    /// required by any compiled function.
    native_memory_size: usize,
    options: SimulatorOptions,
    /// (offset, byte_size) pairs for 4-state variables that need X initialization.
    four_state_inits: Vec<(usize, usize)>,
}

// Safety: JitCode contains Mmap which is Send+Sync after creation.
unsafe impl Send for SharedNativeCode {}
unsafe impl Sync for SharedNativeCode {}

impl SharedNativeCode {
    /// Returns a reference to the memory layout.
    pub fn layout(&self) -> &MemoryLayout {
        &self.layout
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
    code: jit_mem::JitCode,
    trace: Option<emit::NativeFunctionTrace>,
    required_state_size: usize,
}

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

    let (mut sir_eu, merge_provenance) = celox_sir::merge_sir_eu_refs_with_provenance(units);
    let boundaries = merge_provenance.unit_entries[1..].to_vec();
    let verify = |eu: &crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>,
                  phase: &'static str| {
        eu.verify_result().map_err(|source| {
            codegen_err(CodegenError::SirVerification {
                phase: phase.to_string(),
                source,
            })
        })
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
        let code = jit_mem::JitCode::new_named_profiled(
            &empty_result.code,
            label,
            x86_options.diagnostics.perf_map,
        )
        .map_err(|source| codegen_err(CodegenError::NativeMemory { source }))?;
        return Ok(CompiledNativeFunction {
            code,
            trace,
            required_state_size: empty_result.required_state_size as usize,
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
    let code = jit_mem::JitCode::new_named_with_symbols_profiled(
        &emit_result.code,
        label,
        &symbols,
        x86_options.diagnostics.perf_map,
    )
    .map_err(|source| codegen_err(CodegenError::NativeMemory { source }))?;
    Ok(CompiledNativeFunction {
        code,
        trace,
        required_state_size,
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
    mir.push_str("x86-64 disassembly of emitted function:\n");
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

fn compile_program(
    laid_out: &LaidOutProgram,
    options: &SimulatorOptions,
    capture_trace: bool,
) -> Result<(SharedNativeCode, Option<NativeCodegenTrace>), SimulatorError> {
    const MAX_PARALLEL_NATIVE_FUNCTIONS: usize = 4;

    let sir = laid_out;
    let layout = laid_out.layout();
    let (compile_tasks, task_bindings) = collect_ff_compile_tasks(sir);
    let next_task = AtomicUsize::new(0);
    let (comb_jit, mut compiled_ff_codes) = std::thread::scope(|scope| {
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
        .fold(semantic_memory_size, usize::max);
    let comb_func = comb_jit.code.fn_ptr;
    let mut all_jit_codes: Vec<jit_mem::JitCode> = Vec::with_capacity(1 + compiled_ff_codes.len());
    all_jit_codes.push(comb_jit.code);

    // Compile FF units
    let mut next_id = 0usize;
    let mut id_to_addr = Vec::new();
    let mut id_to_event = Vec::new();
    let mut event_map = HashMap::default();
    let mut eval_only_event_map = HashMap::default();
    let mut apply_event_map = HashMap::default();
    let mut addr_to_id = HashMap::default();
    let compiled_ff_cache: HashMap<usize, NativeSimFunc> = compiled_ff_codes
        .iter()
        .map(|(&task_id, compiled)| (task_id, compiled.code.fn_ptr))
        .collect();
    let compile_ff_group = |ff_map: &HashMap<
        AbsoluteAddr,
        Vec<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
    >,
                            label: &'static str,
                            event_map_out: &mut HashMap<AbsoluteAddr, NativeEventRef>,
                            addr_to_id: &mut HashMap<AbsoluteAddr, usize>,
                            compiled_ff_cache: &HashMap<usize, NativeSimFunc>,
                            comb_apply_label: Option<&'static str>,
                            next_id: &mut usize,
                            id_to_addr: &mut Vec<AbsoluteAddr>,
                            id_to_event: &mut Vec<NativeEventRef>|
     -> Result<(), SimulatorError> {
        for addr in ff_map.keys() {
            let canonical = sir.design.events.canonical(*addr);
            if let Some(&event) = event_map_out.get(&canonical) {
                event_map_out.insert(*addr, event);
                continue;
            }

            let task_id = task_bindings[&(label, *addr)];
            let func = compiled_ff_cache[&task_id];
            let comb_apply_func = comb_apply_label
                .map(|label| {
                    let task_id = task_bindings[&(label, *addr)];
                    compiled_ff_cache[&task_id]
                })
                .unwrap_or(func);

            let (id, is_new_id) = if let Some(&id) = addr_to_id.get(&canonical) {
                (id, false)
            } else {
                let id = *next_id;
                *next_id += 1;
                addr_to_id.insert(canonical, id);
                id_to_addr.push(canonical);
                (id, true)
            };

            let event = NativeEventRef {
                func,
                comb_apply_func,
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
        &compiled_ff_cache,
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
        &compiled_ff_cache,
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
        &compiled_ff_cache,
        None,
        &mut next_id,
        &mut id_to_addr,
        &mut id_to_event,
    )?;
    let mut compiled_ff_keys = compiled_ff_codes.keys().copied().collect::<Vec<_>>();
    compiled_ff_keys.sort_unstable();
    for task_id in compiled_ff_keys {
        all_jit_codes.push(
            compiled_ff_codes
                .remove(&task_id)
                .expect("compiled FF key exists")
                .code,
        );
    }
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
        SharedNativeCode {
            comb_func,
            _jit_codes: all_jit_codes,
            event_map,
            eval_only_event_map,
            apply_event_map,
            id_to_addr,
            id_to_event,
            layout: layout.clone(),
            native_memory_size,
            options: options.clone(),
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

impl NativeBackend {
    pub fn new(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<Self, SimulatorError> {
        let (shared, trace) = compile_program(laid_out, options, false)?;
        debug_assert!(trace.is_none());
        let shared = Arc::new(shared);
        Ok(Self::from_shared(shared))
    }

    pub(crate) fn new_with_codegen_trace(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<(Self, NativeCodegenTrace), SimulatorError> {
        let (shared, trace) = compile_program(laid_out, options, true)?;
        let backend = Self::from_shared(Arc::new(shared));
        Ok((
            backend,
            trace.expect("trace-enabled native compilation must return a trace"),
        ))
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
        backend
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
        if self.compiled.options.x86_options.native_tick_loop {
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
