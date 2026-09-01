use std::sync::Arc;

use num_bigint::BigUint;

use crate::{
    HashMap, SimulatorError, SimulatorOptions,
    ir::{AbsoluteAddr, SignalRef},
};

use super::{JitEngine, MemoryLayout, SimulatorErrorCode, get_byte_size};
use super::{RuntimeEventBuffer, memory_image::MemoryImage};
pub type SimFunc = unsafe extern "C" fn(*mut u8) -> u64;

/// Opaque handle to a compiled event (clock / async-reset) function.
/// Holds the JIT-compiled function pointer directly — no indirection.
/// Obtained once via [`JitBackend::resolve_event`] and passed to
/// [`JitBackend::eval_apply_ff_at`] for zero-cost dispatch.
#[derive(Clone, Copy)]
pub struct EventRef {
    pub func: SimFunc,
    pub comb_apply_func: SimFunc,
    pub addr: AbsoluteAddr,
    pub id: usize,
}

impl std::fmt::Debug for EventRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventRef")
            .field("func", &(self.func as usize))
            .field("comb_apply_func", &(self.comb_apply_func as usize))
            .field("addr", &self.addr)
            .field("id", &self.id)
            .finish()
    }
}

impl super::EventHandle for EventRef {
    fn id(&self) -> usize {
        self.id
    }

    fn addr(&self) -> AbsoluteAddr {
        self.addr
    }
}
/// Immutable compilation result that can be shared across simulator instances.
///
/// Contains JIT-compiled function pointers, event maps, and memory layout.
/// The `JitEngine` (which owns the JITModule code pages) is kept alive here.
///
/// # Safety
///
/// After `JITModule::finalize_definitions()`, the compiled code memory is
/// immutable. The function pointers (`SimFunc`) are plain pointers to these
/// code pages and remain valid across threads for the lifetime of this struct.
pub struct SharedJitCode {
    _engine: JitEngine,
    pub(crate) comb_func: SimFunc,
    pub(crate) event_map: HashMap<AbsoluteAddr, EventRef>,
    pub(crate) eval_only_event_map: HashMap<AbsoluteAddr, EventRef>,
    pub(crate) apply_event_map: HashMap<AbsoluteAddr, EventRef>,
    pub(crate) id_to_addr: Vec<AbsoluteAddr>,
    pub(crate) id_to_event: Vec<EventRef>,
    pub(crate) layout: MemoryLayout,
    pub(crate) options: SimulatorOptions,
    /// Pre-computed 4-state init regions: (offset, allocated_size) for stable
    /// and working regions.
    four_state_inits: Vec<(usize, usize)>,
}

// SAFETY: After JITModule finalization, compiled code is immutable.
// Function pointers are valid across threads for SharedJitCode's lifetime.
// We never call mutating methods on JitEngine after construction.
unsafe impl Send for SharedJitCode {}
unsafe impl Sync for SharedJitCode {}

impl SharedJitCode {
    /// Returns a reference to the memory layout.
    pub fn layout(&self) -> &MemoryLayout {
        &self.layout
    }
}

pub struct JitBackend {
    shared: Arc<SharedJitCode>,
    memory: MemoryImage,
    runtime_event_buffer: Arc<RuntimeEventBuffer>,
    comb_capture_enabled: Vec<u8>,
    /// Cached from `shared.comb_func` to avoid Arc dereference on the hot path.
    comb_func: SimFunc,
}

/// Cranelift-only lowering plan for an oversized `eval_comb` function.
///
/// This is deliberately constructed at the backend boundary: it contains
/// concrete CLIF chunking decisions and must never become part of SIR or the
/// backend-neutral state layout.
enum CraneliftEvalCombPlan {
    Unsplit,
    TailCallChunks(Vec<celox_backend_cranelift::tail_call_split::TailCallChunk>),
    MemorySpilled(celox_backend_cranelift::tail_call_split::MemorySpilledPlan),
}

impl CraneliftEvalCombPlan {
    fn build(sir: &crate::ir::LaidOutProgram, options: &SimulatorOptions) -> Self {
        if !options.cranelift_options.tail_call_split {
            return Self::Unsplit;
        }

        let timing = options.diagnostics.optimizer_timing;
        if timing {
            use celox_backend_cranelift::cost_model::{
                CLIF_INST_THRESHOLD, VREG_VALUE_THRESHOLD, estimate_eu_cost,
                estimate_eu_value_count,
            };
            for (i, eu) in sir.sir.eval_comb.iter().enumerate() {
                let inst_cost = estimate_eu_cost(eu, options.four_state);
                let value_count = estimate_eu_value_count(eu, options.four_state);
                tracing::debug!(
                    "[split-check] eval_comb eu[{i}]: blocks={} insts={} clif_cost={inst_cost}/{CLIF_INST_THRESHOLD} values={value_count}/{VREG_VALUE_THRESHOLD}",
                    eu.blocks.len(),
                    eu.blocks
                        .values()
                        .map(|block| block.instructions.len())
                        .sum::<usize>(),
                );
            }
        }

        let split_start = timing.then(crate::timing::now);
        use celox_backend_cranelift::tail_call_split;
        if let Some(chunks) =
            tail_call_split::split_if_needed(&sir.sir.eval_comb, options.four_state)
        {
            if let Some(start) = split_start {
                tracing::debug!(
                    "[split] TailCallChunks: {} chunks, took {:?}",
                    chunks.len(),
                    start.elapsed()
                );
            }
            Self::TailCallChunks(chunks)
        } else if let Some(plan) =
            tail_call_split::split_if_needed_spilled(&sir.sir.eval_comb, options.four_state)
        {
            if let Some(start) = split_start {
                tracing::debug!(
                    "[split] MemorySpilled: {} chunks, scratch={}B, took {:?}",
                    plan.chunks.len(),
                    plan.scratch_bytes,
                    start.elapsed()
                );
            }
            Self::MemorySpilled(plan)
        } else {
            Self::Unsplit
        }
    }

    fn scratch_size(&self) -> usize {
        match self {
            Self::MemorySpilled(plan) => plan.scratch_bytes,
            Self::Unsplit | Self::TailCallChunks(_) => 0,
        }
    }
}

impl JitBackend {
    pub fn new(
        laid_out: &crate::ir::LaidOutProgram,
        options: &SimulatorOptions,
        trace: Option<&mut crate::debug::CompilationTrace>,
    ) -> Result<Self, crate::SimulatorError> {
        let shared = Arc::new(Self::compile(laid_out, options, trace)?);
        Ok(Self::from_shared(shared))
    }

    /// Build the shared JIT code from finalized SIR and layout.
    pub(crate) fn compile(
        laid_out: &crate::ir::LaidOutProgram,
        options: &SimulatorOptions,
        mut trace: Option<&mut crate::debug::CompilationTrace>,
    ) -> Result<SharedJitCode, crate::SimulatorError> {
        let sir = laid_out;

        // Auto-select SinglePass RA for large designs where Backtracking RA's
        // superlinear compile time would dominate. The threshold is half the
        // CLIF instruction limit — at this size, code quality differences
        // between allocators are negligible compared to compile time savings.
        let mut options = options.clone();
        {
            use celox_backend_cranelift::cost_model::*;
            let _comb_cost: usize = sir
                .sir
                .eval_comb
                .iter()
                .map(|eu| estimate_eu_cost(eu, options.four_state))
                .sum();
            let comb_vregs: usize = sir
                .sir
                .eval_comb
                .iter()
                .map(|eu| estimate_eu_value_count(eu, options.four_state))
                .sum();
            // estimate_eu_value_count under-counts by ~2x vs actual CLIF values.
            // Backtracking RA scales superlinearly with vreg count; switch to
            // SinglePass when estimated vregs suggest actual count would be large.
            if comb_vregs > VREG_VALUE_THRESHOLD / 4
                && matches!(
                    options.cranelift_options.regalloc_algorithm,
                    crate::backend::RegallocAlgorithm::Backtracking
                )
            {
                options.cranelift_options.regalloc_algorithm =
                    crate::backend::RegallocAlgorithm::SinglePass;
            }
        }

        let eval_comb_plan = CraneliftEvalCombPlan::build(sir, &options);
        let layout = laid_out
            .layout()
            .clone()
            .with_backend_scratch(eval_comb_plan.scratch_size());

        #[cfg(all(
            feature = "host-runtime",
            target_arch = "x86_64",
            not(feature = "arm64-codegen")
        ))]
        let layout_for_mir = if options.trace.mir {
            Some(layout.clone())
        } else {
            None
        };
        let compile_options = celox_backend_cranelift::CompileOptions {
            four_state: options.four_state,
            emit_triggers: options.emit_triggers,
            cranelift: options.cranelift_options,
        };
        let mut engine = JitEngine::new(layout, &compile_options).map_err(SimulatorError::from)?;

        let mut pre_clif_buf = String::new();
        let mut post_clif_buf = String::new();
        let mut native_buf = String::new();

        let (pre_clif_ptr, post_clif_ptr, native_ptr) = if trace.is_some() {
            (
                options
                    .trace
                    .pre_optimized_clif
                    .then_some(&mut pre_clif_buf),
                options
                    .trace
                    .post_optimized_clif
                    .then_some(&mut post_clif_buf),
                options.trace.native.then_some(&mut native_buf),
            )
        } else {
            (None, None, None)
        };

        // Batch compile eval_comb, using the backend-local chunking plan when
        // the combined CLIF would exceed Cranelift's instruction limit.
        let res = match &eval_comb_plan {
            CraneliftEvalCombPlan::MemorySpilled(plan) => {
                engine.compile_spilled_chunks(plan, pre_clif_ptr, post_clif_ptr, native_ptr)
            }
            CraneliftEvalCombPlan::TailCallChunks(chunks) => {
                engine.compile_chunks(chunks, pre_clif_ptr, post_clif_ptr, native_ptr)
            }
            CraneliftEvalCombPlan::Unsplit => {
                engine.compile_units(&sir.sir.eval_comb, pre_clif_ptr, post_clif_ptr, native_ptr)
            }
        };

        if let Some(t) = trace.as_deref_mut() {
            if options.trace.pre_optimized_clif {
                let mut full_clif = String::new();
                full_clif.push_str("=========================================\n");
                full_clif.push_str("  Cranelift IR (CLIF) Dump (Pre-Optimized)\n");
                full_clif.push_str("=========================================\n\n");
                full_clif.push_str(&pre_clif_buf);
                t.pre_optimized_clif = Some(full_clif);
            }
            if options.trace.post_optimized_clif {
                let mut full_clif = String::new();
                full_clif.push_str("=========================================\n");
                full_clif.push_str("  Cranelift IR (CLIF) Dump (Post-Optimized)\n");
                full_clif.push_str("=========================================\n\n");
                full_clif.push_str(&post_clif_buf);
                t.post_optimized_clif = Some(full_clif);
            }
            if options.trace.native {
                let mut full_native = String::new();
                full_native.push_str("=========================================\n");
                full_native.push_str("  Native Machine Code Dump\n");
                full_native.push_str("=========================================\n\n");
                full_native.push_str(&native_buf);
                t.native = Some(full_native);
            }
            #[cfg(all(
                feature = "host-runtime",
                target_arch = "x86_64",
                not(feature = "arm64-codegen")
            ))]
            if options.trace.mir {
                use super::native::isel::lower_execution_unit;
                use super::native::mir_legalize;
                use super::native::mir_opt;
                use super::native::regalloc::run_regalloc;
                let mut mir_output = String::new();
                let layout_ref = layout_for_mir.as_ref().unwrap();

                mir_output.push_str("=== MIR (eval_comb) ===\n");
                for (idx, eu) in sir.sir.eval_comb.iter().enumerate() {
                    let mut mfunc = lower_execution_unit(eu, layout_ref, options.four_state);
                    mir_legalize::legalize(&mut mfunc);
                    mir_opt::optimize(&mut mfunc);
                    mir_output.push_str(&format!("Execution Unit {idx} (before regalloc):\n"));
                    mir_output.push_str(&format!("{mfunc}\n"));
                    let ra = match run_regalloc(&mut mfunc) {
                        Ok(allocation) => allocation,
                        Err(error) => {
                            mir_output.push_str(&format!("  regalloc error: {error}\n\n"));
                            continue;
                        }
                    };
                    mir_output.push_str(&format!("Execution Unit {idx} (after regalloc):\n"));
                    mir_output.push_str(&format!("{mfunc}"));
                    mir_output.push_str("  Register assignment:\n");
                    for (vreg, preg) in ra.assignment.sorted_entries() {
                        mir_output.push_str(&format!("    {vreg} -> {preg}\n"));
                    }
                    // Emit x86-64 and disassemble
                    match super::native::emit::emit(&mfunc, &ra.assignment, ra.spill_frame_size) {
                        Ok(result) => {
                            mir_output.push_str("  x86-64 disassembly:\n");
                            mir_output.push_str(&super::native::emit::disassemble(
                                &result.code[..result.text_size],
                                0,
                            ));
                        }
                        Err(e) => {
                            mir_output.push_str(&format!("  emit error: {e}\n"));
                        }
                    }
                    mir_output.push('\n');
                }
                for (addr, units) in &sir.sir.eval_apply_ffs {
                    mir_output.push_str(&format!(
                        "=== MIR (eval_apply_ffs) Trigger: {} ===\n",
                        sir.get_path(addr)
                    ));
                    for (idx, eu) in units.iter().enumerate() {
                        let mut mfunc = lower_execution_unit(eu, layout_ref, options.four_state);
                        mir_legalize::legalize(&mut mfunc);
                        mir_output.push_str(&format!("Execution Unit {idx} (before regalloc):\n"));
                        mir_output.push_str(&format!("{mfunc}\n"));
                        let ra = match run_regalloc(&mut mfunc) {
                            Ok(allocation) => allocation,
                            Err(error) => {
                                mir_output.push_str(&format!("  regalloc error: {error}\n\n"));
                                continue;
                            }
                        };
                        mir_output.push_str(&format!("Execution Unit {idx} (after regalloc):\n"));
                        mir_output.push_str(&format!("{mfunc}"));
                        mir_output.push_str("  Register assignment:\n");
                        for (vreg, preg) in ra.assignment.sorted_entries() {
                            mir_output.push_str(&format!("    {vreg} -> {preg}\n"));
                        }
                        mir_output.push('\n');
                    }
                }
                t.mir = Some(mir_output);
            }
        }

        let comb_code_ptr = res.map_err(SimulatorError::from)?;

        let mut next_id = 0;
        let mut addr_to_id = HashMap::default();
        let mut id_to_addr = Vec::new();
        let mut ff_funcs = Vec::new();

        let mut compile_ffs = |ff_map: &HashMap<
            AbsoluteAddr,
            Vec<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
        >,
                               addr_to_id: &mut HashMap<AbsoluteAddr, usize>,
                               next_id: &mut usize,
                               id_to_addr: &mut Vec<AbsoluteAddr>|
         -> Result<
            HashMap<AbsoluteAddr, EventRef>,
            crate::SimulatorError,
        > {
            let mut event_map = HashMap::default();
            for (clock, units) in ff_map {
                let id = *addr_to_id.entry(*clock).or_insert_with(|| {
                    let id = *next_id;
                    *next_id += 1;
                    id_to_addr.push(*clock);
                    id
                });

                let mut ff_pre_clif_buf = String::new();
                let mut ff_post_clif_buf = String::new();
                let mut ff_native_buf = String::new();
                let (ff_pre_clif_ptr, ff_post_clif_ptr, ff_native_ptr) = if trace.is_some() {
                    (
                        options
                            .trace
                            .pre_optimized_clif
                            .then_some(&mut ff_pre_clif_buf),
                        options
                            .trace
                            .post_optimized_clif
                            .then_some(&mut ff_post_clif_buf),
                        options.trace.native.then_some(&mut ff_native_buf),
                    )
                } else {
                    (None, None, None)
                };

                let res =
                    engine.compile_units(units, ff_pre_clif_ptr, ff_post_clif_ptr, ff_native_ptr);

                if let Some(t) = trace.as_deref_mut() {
                    if options.trace.pre_optimized_clif {
                        t.pre_optimized_clif
                            .get_or_insert_with(String::new)
                            .push_str(&ff_pre_clif_buf);
                    }
                    if options.trace.post_optimized_clif {
                        t.post_optimized_clif
                            .get_or_insert_with(String::new)
                            .push_str(&ff_post_clif_buf);
                    }
                    if options.trace.native {
                        t.native
                            .get_or_insert_with(String::new)
                            .push_str(&ff_native_buf);
                    }
                }

                let ptr = res.map_err(SimulatorError::from)?;
                let func: SimFunc = unsafe { std::mem::transmute(ptr) };
                ff_funcs.push(func);
                event_map.insert(
                    *clock,
                    EventRef {
                        func,
                        comb_apply_func: func,
                        addr: *clock,
                        id,
                    },
                );
            }
            Ok(event_map)
        };

        let mut event_map = compile_ffs(
            &sir.sir.eval_apply_ffs,
            &mut addr_to_id,
            &mut next_id,
            &mut id_to_addr,
        )?;
        let mut eval_only_event_map = compile_ffs(
            &sir.sir.eval_only_ffs,
            &mut addr_to_id,
            &mut next_id,
            &mut id_to_addr,
        )?;
        let mut apply_event_map = compile_ffs(
            &sir.sir.apply_ffs,
            &mut addr_to_id,
            &mut next_id,
            &mut id_to_addr,
        )?;

        // Release borrows captured by compile_ffs so engine is available again.
        // (Using `let _` instead of `drop()` to avoid clippy::drop_non_drop.)
        let _ = compile_ffs;

        // Deferred testbench ticks require the scheduler's combined comb/FF
        // program. Calling independently compiled comb and FF functions can
        // observe a different pre-edge snapshot around reset and NBA regions.
        for (clock, ff_units) in &sir.sir.eval_apply_ffs {
            let mut combined_units;
            let units = if let Some(fused) = sir.sir.eval_comb_apply_ffs.get(clock) {
                fused.as_slice()
            } else {
                combined_units = sir.sir.eval_comb.clone();
                combined_units.extend(ff_units.iter().cloned());
                combined_units.as_slice()
            };
            let ptr = engine
                .compile_units(units, None, None, None)
                .map_err(SimulatorError::from)?;
            let comb_apply_func: SimFunc = unsafe { std::mem::transmute(ptr) };
            event_map
                .get_mut(clock)
                .expect("compiled clock event is present")
                .comb_apply_func = comb_apply_func;
        }

        // Insert clock_domains aliases so every event signal resolves
        for (alias, canonical) in &sir.design.events.aliases {
            if let Some(&ev) = event_map.get(canonical) {
                event_map.insert(*alias, ev);
            }
            if let Some(&ev) = eval_only_event_map.get(canonical) {
                eval_only_event_map.insert(*alias, ev);
            }
            if let Some(&ev) = apply_event_map.get(canonical) {
                apply_event_map.insert(*alias, ev);
            }
        }

        let id_to_event: Vec<EventRef> = id_to_addr.iter().map(|addr| event_map[addr]).collect();

        let comb_func: SimFunc = unsafe { std::mem::transmute(comb_code_ptr) };

        debug_assert_eq!(
            engine.layout().working_base_offset,
            (engine.layout().total_size + 7) & !7
        );
        debug_assert_eq!(
            engine.layout().merged_total_size,
            (engine.layout().scratch_base_offset + engine.layout().scratch_size + 7) & !7
        );

        // Pre-compute 4-state initialization regions
        let mut four_state_inits = Vec::new();
        if options.four_state {
            for (addr, &offset) in &engine.layout().offsets {
                let width = engine.layout().widths[addr];
                let is_4state = sir
                    .design
                    .state_objects
                    .get(addr)
                    .map(|metadata| metadata.is_4state)
                    .unwrap_or(false);

                if is_4state {
                    let allocated_size = super::get_byte_size(width);
                    four_state_inits.push((offset, allocated_size));
                }
            }
            for (addr, &rel_offset) in &engine.layout().working_offsets {
                let offset = engine.layout().working_base_offset + rel_offset;
                let width = engine.layout().widths[addr];
                let is_4state = sir
                    .design
                    .state_objects
                    .get(addr)
                    .map(|metadata| metadata.is_4state)
                    .unwrap_or(false);

                if is_4state {
                    let allocated_size = super::get_byte_size(width);
                    four_state_inits.push((offset, allocated_size));
                }
            }
        }

        let layout = engine.layout().clone();
        let options = options.clone();

        Ok(SharedJitCode {
            _engine: engine,
            comb_func,
            event_map,
            eval_only_event_map,
            apply_event_map,
            id_to_addr,
            id_to_event,
            layout,
            options,
            four_state_inits,
        })
    }

    /// Create a new backend instance from shared compiled code.
    ///
    /// Allocates a fresh simulation memory buffer and initializes 4-state
    /// regions. The compiled function pointers are shared across instances.
    pub fn from_shared(shared: Arc<SharedJitCode>) -> Self {
        let num_u64 = shared.layout.merged_total_size.div_ceil(8);
        let mut memory = MemoryImage::zeroed(num_u64);
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

        let comb_func = shared.comb_func;
        let mut backend = Self {
            shared,
            memory,
            runtime_event_buffer,
            comb_capture_enabled,
            comb_func,
        };
        backend.install_event_buffers();
        backend
    }

    /// Build a backend from compiled code plus an existing live simulation
    /// state, e.g. when a tiered simulation promotes from the interpreter.
    ///
    /// The caller must guarantee the state was produced against the same
    /// laid-out program (identical packed layout), and that the event buffer
    /// `Arc` is the one referenced by the state header so its pointer stays
    /// valid without reinstallation.
    pub(crate) fn adopt_shared_with_state(
        shared: Arc<SharedJitCode>,
        memory: MemoryImage,
        runtime_event_buffer: Arc<RuntimeEventBuffer>,
        comb_capture_enabled: Vec<u8>,
    ) -> Self {
        // A MemorySpilled compile plan extends the compiled layout with
        // backend scratch beyond the semantic state the interpreter owned.
        // Grow the transferred image so generated spilled chunks never touch
        // memory past the allocation; the tail is fresh zeroed scratch.
        let target_words = shared.layout.merged_total_size.div_ceil(8);
        let mut memory = memory;
        if memory.len_words() < target_words {
            memory.resize_zeroed_within_capacity(target_words);
        }
        let comb_func = shared.comb_func;
        Self {
            shared,
            memory,
            runtime_event_buffer,
            comb_capture_enabled,
            comb_func,
        }
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

    /// Returns the shared compiled code, allowing it to be reused for
    /// creating additional simulator instances without recompilation.
    pub fn shared_code(&self) -> Arc<SharedJitCode> {
        Arc::clone(&self.shared)
    }

    #[inline]
    fn run_sim_func(&mut self, func: SimFunc) -> Result<(), SimulatorErrorCode> {
        let ptr = self.memory.as_mut_ptr() as *mut u8;
        self.run_sim_func_at(func, ptr)
    }

    #[inline]
    fn run_sim_func_at(&mut self, func: SimFunc, ptr: *mut u8) -> Result<(), SimulatorErrorCode> {
        let res = unsafe { (func)(ptr) };
        match res {
            0 => Ok(()),
            code if code > 0 => Err(SimulatorErrorCode::DetectedTrueLoopCode(code as i64)),
            _ => unreachable!(),
        }
    }
    /// Execute combinational logic
    pub fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(self.comb_func)
    }

    /// Resolves an `AbsoluteAddr` into a performance-optimized [`SignalRef`].
    /// This handle allows for direct memory access without `HashMap` lookups.
    pub fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        let offset = self.shared.layout.offsets[addr];
        let width = self.shared.layout.widths[addr];
        let is_4state = self.shared.layout.is_4states[addr];
        SignalRef {
            offset,
            width,
            is_4state,
            array_layout: None,
        }
    }

    /// Set value for a variable using a pre-resolved [`SignalRef`].
    pub fn set<T: Copy>(&mut self, signal: SignalRef, value: T) {
        let allocated_size = get_byte_size(signal.width);
        let provided_size = std::mem::size_of::<T>();
        let clear_mask = self.shared.options.four_state && signal.is_4state;

        assert!(provided_size <= allocated_size);

        unsafe {
            let base_ptr = (self.memory.as_mut_ptr() as *mut u8).add(signal.offset);
            if !clear_mask && allocated_size == 1 {
                let raw = *(&value as *const T as *const u8);
                let byte = if signal.width < 8 {
                    raw & ((1u8 << signal.width) - 1)
                } else {
                    raw
                };
                *base_ptr = byte;
                return;
            }

            std::ptr::write_bytes(base_ptr, 0, allocated_size);
            let ptr = base_ptr as *mut T;
            std::ptr::write_unaligned(ptr, value);

            if clear_mask {
                let mask_ptr = base_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0, allocated_size);
            }
        }
    }

    /// Set value for a variable using a pre-resolved [`SignalRef`] and `BigUint`.
    pub fn set_wide(&mut self, signal: SignalRef, value: BigUint) {
        let allocated_size = get_byte_size(signal.width);
        let mut bytes = value.to_bytes_le();

        if bytes.len() > allocated_size {
            bytes.truncate(allocated_size);
        } else {
            bytes.resize(allocated_size, 0u8);
        }

        unsafe {
            let dst_ptr: *mut u8 = self.memory.as_mut_ptr().cast();
            let dst_ptr = dst_ptr.add(signal.offset);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst_ptr, allocated_size);

            if self.shared.options.four_state && signal.is_4state {
                let mask_ptr = dst_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0, allocated_size);
            }
        }
    }

    /// Get value of a variable using a pre-resolved [`SignalRef`].
    pub fn get(&self, signal: SignalRef) -> BigUint {
        let byte_size = super::get_byte_size(signal.width);
        let ptr: *const u8 = unsafe { (self.memory.as_ptr() as *const u8).add(signal.offset) };
        let byte_slice = unsafe { std::slice::from_raw_parts(ptr, byte_size) };
        let mut val = BigUint::from_bytes_le(byte_slice);

        let extra_bits = byte_size * 8 - signal.width;
        if extra_bits > 0 {
            let mask = (BigUint::from(1u32) << signal.width) - 1u32;
            val &= mask;
        }
        val
    }

    /// Get value of a variable as a specific integer type without creating a `BigUint`.
    /// The type `T` must be large enough to hold the signal width.
    pub fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        let byte_size = super::get_byte_size(signal.width);
        let ptr: *const u8 = unsafe { (self.memory.as_ptr() as *const u8).add(signal.offset) };
        let byte_slice = unsafe { std::slice::from_raw_parts(ptr, byte_size) };

        let provided_size = std::mem::size_of::<T>();
        assert!(
            byte_size <= provided_size,
            "Provided type is too small for signal width"
        );

        let mut val = T::default();
        unsafe {
            let val_ptr = &mut val as *mut T as *mut u8;
            std::ptr::copy_nonoverlapping(byte_slice.as_ptr(), val_ptr, byte_size);
        }

        // Mask extra bits if signal width is not a multiple of 8
        let extra_bits = byte_size * 8 - signal.width;
        if extra_bits > 0 {
            // This masking is tricky with generic T.
            // Since we mostly use this for clock edges (usually 1-bit),
            // and provided_size is likely 1, 8, or 64, we can handle common cases.
            if provided_size == 1 {
                let mask = (1u8 << (8 - extra_bits)) - 1;
                let v = unsafe { std::mem::transmute_copy::<T, u8>(&val) };
                val = unsafe { std::mem::transmute_copy::<u8, T>(&(v & mask)) };
            } else if provided_size == 8 {
                let mask = (1u64 << signal.width) - 1;
                let v = unsafe { std::mem::transmute_copy::<T, u64>(&val) };
                val = unsafe { std::mem::transmute_copy::<u64, T>(&(v & mask)) };
            }
        }
        val
    }

    /// Set 4-state value for a variable using a pre-resolved [`SignalRef`].
    ///
    /// Uses IEEE 1800 encoding:
    /// - `(v=0, m=0)` → 0
    /// - `(v=1, m=0)` → 1
    /// - `(v=1, m=1)` → X (unknown)
    /// - `(v=0, m=1)` → Z (high-impedance)
    pub fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        let allocated_size = get_byte_size(signal.width);

        let mut v_bytes = value.to_bytes_le();
        if v_bytes.len() > allocated_size {
            v_bytes.truncate(allocated_size);
        } else {
            v_bytes.resize(allocated_size, 0u8);
        }

        unsafe {
            let dst_ptr: *mut u8 = self.memory.as_mut_ptr().cast();
            std::ptr::copy_nonoverlapping(
                v_bytes.as_ptr(),
                dst_ptr.add(signal.offset),
                allocated_size,
            );

            if self.shared.options.four_state && signal.is_4state {
                let mut m_bytes = mask.to_bytes_le();
                if m_bytes.len() > allocated_size {
                    m_bytes.truncate(allocated_size);
                } else {
                    m_bytes.resize(allocated_size, 0u8);
                }

                std::ptr::copy_nonoverlapping(
                    m_bytes.as_ptr(),
                    dst_ptr.add(signal.offset + allocated_size),
                    allocated_size,
                );
            }
        }
    }

    /// Get 4-state value for a variable using a pre-resolved [`SignalRef`].
    ///
    /// Returns `(value, mask)` using IEEE 1800 encoding:
    /// - `(v=0, m=0)` → 0
    /// - `(v=1, m=0)` → 1
    /// - `(v=1, m=1)` → X (unknown)
    /// - `(v=0, m=1)` → Z (high-impedance)
    pub fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        let byte_size = get_byte_size(signal.width);
        let v_ptr: *const u8 = unsafe { (self.memory.as_ptr() as *const u8).add(signal.offset) };
        let v_slice = unsafe { std::slice::from_raw_parts(v_ptr, byte_size) };
        let mut v_val = BigUint::from_bytes_le(v_slice);

        let mut m_val = if self.shared.options.four_state && signal.is_4state {
            let m_ptr: *const u8 = unsafe { v_ptr.add(byte_size) };
            let m_slice = unsafe { std::slice::from_raw_parts(m_ptr, byte_size) };
            BigUint::from_bytes_le(m_slice)
        } else {
            BigUint::from(0u32)
        };

        let extra_bits = byte_size * 8 - signal.width;
        if extra_bits > 0 {
            let bitmask = (BigUint::from(1u32) << signal.width) - 1u32;
            v_val &= &bitmask;
            m_val &= &bitmask;
        }

        (v_val, m_val)
    }

    /// Resolve an `AbsoluteAddr` (clock or async-reset signal) into an
    /// [`EventRef`] handle.  This does a one-time `HashMap` lookup; the
    /// returned handle can then be passed to [`Self::eval_apply_ff_at`] for zero-cost
    /// direct function-pointer dispatch.
    pub fn resolve_event(&self, addr: &AbsoluteAddr) -> EventRef {
        self.shared.event_map[addr]
    }

    pub fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.shared.event_map.get(addr).copied()
    }

    pub fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.shared.eval_only_event_map.get(addr).copied()
    }

    pub fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.shared.apply_event_map.get(addr).copied()
    }

    pub fn eval_apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(event.func)
    }

    pub fn eval_only_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(event.func)
    }

    pub fn apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(event.func)
    }

    /// Returns a raw pointer to the JIT memory and its total size in bytes.
    pub fn memory_as_ptr(&self) -> (*const u8, usize) {
        let size = self.shared.layout.merged_total_size;
        (self.memory.as_ptr() as *const u8, size)
    }

    /// Returns a mutable raw pointer to the JIT memory and its total size in bytes.
    pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        let size = self.shared.layout.merged_total_size;
        (self.memory.as_mut_ptr() as *mut u8, size)
    }

    pub fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize) {
        (
            self.runtime_event_buffer.as_ptr(),
            self.runtime_event_buffer.byte_size(),
        )
    }

    pub fn runtime_event_buffer(&self) -> Arc<RuntimeEventBuffer> {
        Arc::clone(&self.runtime_event_buffer)
    }

    pub fn set_comb_capture_event_enabled(&mut self, active_sites: &[bool]) {
        self.comb_capture_enabled.fill(0);
        for (idx, active) in active_sites.iter().copied().enumerate() {
            if active && idx < self.comb_capture_enabled.len() {
                self.comb_capture_enabled[idx] = 1;
            }
        }
    }

    /// Returns the stable region size in bytes.
    pub fn stable_region_size(&self) -> usize {
        self.shared.layout.total_size
    }

    /// Returns a reference to the memory layout.
    pub fn layout(&self) -> &MemoryLayout {
        &self.shared.layout
    }

    /// Returns the `id_to_addr` mapping (event ID → AbsoluteAddr).
    pub fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        &self.shared.id_to_addr
    }

    /// Returns the `id_to_event` mapping (event ID → EventRef).
    pub fn id_to_event_slice(&self) -> &[EventRef] {
        &self.shared.id_to_event
    }

    pub fn num_events(&self) -> usize {
        let mut max_id = 0;
        for ev in self.shared.event_map.values() {
            max_id = max_id.max(ev.id);
        }
        for ev in self.shared.eval_only_event_map.values() {
            max_id = max_id.max(ev.id);
        }
        for ev in self.shared.apply_event_map.values() {
            max_id = max_id.max(ev.id);
        }
        if self.shared.event_map.is_empty()
            && self.shared.eval_only_event_map.is_empty()
            && self.shared.apply_event_map.is_empty()
        {
            0
        } else {
            max_id + 1
        }
    }

    /// Clears the triggered bits bitset in JIT memory.
    pub fn clear_triggered_bits(&mut self) {
        let base_ptr = self.memory.as_mut_ptr() as *mut u8;
        let triggered_bits_ptr = unsafe { base_ptr.add(self.shared.layout.triggered_bits_offset) };
        let total_size = self.shared.layout.triggered_bits_total_size;
        unsafe {
            std::ptr::write_bytes(triggered_bits_ptr, 0, total_size);
        }
    }

    /// Manually marks a trigger bit as triggered in JIT memory.
    pub fn mark_triggered_bit(&mut self, id: usize) {
        let byte_idx = id / 8;
        let bit_idx = id % 8;
        let base_ptr = self.memory.as_mut_ptr() as *mut u8;
        let triggered_bits_ptr = unsafe { base_ptr.add(self.shared.layout.triggered_bits_offset) };
        unsafe {
            let byte_ptr = triggered_bits_ptr.add(byte_idx);
            *byte_ptr |= 1 << bit_idx;
        }
    }

    /// Reads back the triggered bits bitset and returns it as a BitSet.
    pub fn get_triggered_bits(&self) -> bit_set::BitSet {
        let mut bits = bit_set::BitSet::with_capacity(self.num_events());
        let base_ptr = self.memory.as_ptr() as *const u8;
        let triggered_bits_ptr = unsafe { base_ptr.add(self.shared.layout.triggered_bits_offset) };
        let total_size = self.shared.layout.triggered_bits_total_size;

        for i in 0..total_size {
            let byte = unsafe { *triggered_bits_ptr.add(i) };
            if byte != 0 {
                for j in 0..8 {
                    if (byte & (1 << j)) != 0 {
                        bits.insert(i * 8 + j);
                    }
                }
            }
        }
        bits
    }
}

impl super::SimBackend for JitBackend {
    type Event = EventRef;

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        self.eval_comb()
    }

    fn eval_apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.eval_apply_ff_at(event)
    }

    fn eval_comb_apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(event.comb_apply_func)
    }

    fn eval_only_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.eval_only_ff_at(event)
    }

    fn apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.apply_ff_at(event)
    }

    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        self.resolve_signal(addr)
    }

    fn resolve_event(&self, addr: &AbsoluteAddr) -> EventRef {
        self.resolve_event(addr)
    }

    fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.resolve_event_opt(addr)
    }

    fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.resolve_eval_only_event(addr)
    }

    fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.resolve_apply_event(addr)
    }

    fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
        self.set(signal, val)
    }

    fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
        self.set_wide(signal, val)
    }

    fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint) {
        self.set_four_state(signal, val, mask)
    }

    fn get(&self, signal: SignalRef) -> BigUint {
        self.get(signal)
    }

    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        self.get_as(signal)
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        self.get_four_state(signal)
    }

    fn memory_as_ptr(&self) -> (*const u8, usize) {
        self.memory_as_ptr()
    }

    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        self.memory_as_mut_ptr()
    }

    fn memory_owner(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        Some(self.memory.owner())
    }

    fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize) {
        self.runtime_event_buffer_as_ptr()
    }

    fn runtime_event_buffer(&self) -> Option<Arc<RuntimeEventBuffer>> {
        Some(self.runtime_event_buffer())
    }

    fn set_comb_capture_event_enabled(&mut self, active_sites: &[bool]) {
        self.set_comb_capture_event_enabled(active_sites);
    }

    fn stable_region_size(&self) -> usize {
        self.stable_region_size()
    }

    fn layout(&self) -> &super::MemoryLayout {
        self.layout()
    }

    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        self.id_to_addr_slice()
    }

    fn id_to_event_slice(&self) -> &[EventRef] {
        self.id_to_event_slice()
    }

    fn num_events(&self) -> usize {
        self.num_events()
    }

    fn clear_triggered_bits(&mut self) {
        self.clear_triggered_bits()
    }

    fn mark_triggered_bit(&mut self, id: usize) {
        self.mark_triggered_bit(id)
    }

    fn get_triggered_bits(&self) -> bit_set::BitSet {
        self.get_triggered_bits()
    }
}
