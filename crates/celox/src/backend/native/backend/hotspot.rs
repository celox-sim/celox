//! Code of the hotspot scheduler tier.

use super::*;

/// Allocated scheduler MIR without a guessed native callee scratch extent.
#[cfg(hotspot_jit)]
pub(crate) struct PreparedHotspotScheduler {
    pub(super) program: Arc<LaidOutProgram>,
    /// Its inline budget outlined units a later stage can inline.
    pub(crate) budgeted: bool,
    pub(super) allocated: emit::AllocatedFunction,
    // Only the shared Comb entry is unresolved. All other outlined kernels
    // can compile independently while that entry is still on the worker.
    pub(super) callees: Vec<Option<Arc<HotspotCode>>>,
}

#[cfg(hotspot_jit)]
impl PreparedHotspotScheduler {
    /// Optimize, allocate and compile independent callees while Comb compiles.
    /// Only linking and emission wait for its actual scratch extent.
    pub(crate) fn new(
        program: Arc<LaidOutProgram>,
        options: &SimulatorOptions,
        cancel: &CompileCancel,
        inline_budget: Option<usize>,
    ) -> Result<Self, SimulatorError> {
        let crate::scheduler::HotspotScheduler {
            unit,
            callees: outlined,
            budgeted,
        } = crate::scheduler::hotspot_scheduler(&program, inline_budget, Some(cancel)).map_err(
            |error| {
                if cancel.is_cancelled() {
                    crate::backend::compile_cancel::cancelled_error()
                } else {
                    codegen_message(error)
                }
            },
        )?;
        HotspotCode::validate_units(std::slice::from_ref(&unit))?;
        let mut native = options.x86_options.clone();
        native.baseline = false;
        native.native_tick_loop = false;
        let unit = prepare_merged_sir(
            &[&unit],
            program.layout(),
            options.four_state,
            "hotspot",
            NativeMergeKind::Partial,
            None,
            &options.optimize_options.diagnostics,
            Some(cancel),
            native.baseline,
            native.function.is_some(),
        )?;
        #[cfg(target_arch = "x86_64")]
        let allocated = emit::allocate_owned_prepared_eu(
            unit,
            program.layout(),
            options.four_state,
            "hotspot",
            &native,
            None,
            || cancel.is_cancelled(),
        )
        .map_err(hotspot_pipeline_error)?;
        // No tick loop and no function units: `validate_units` rejects calls.
        #[cfg(target_arch = "aarch64")]
        let allocated = emit::allocate_prepared_eu_with_functions(
            &unit,
            program.layout(),
            options.four_state,
            "hotspot",
            native.native_tick_loop,
            None,
            None,
            || cancel.is_cancelled(),
        )
        .map_err(hotspot_pipeline_error)?;
        let mut callees = Vec::with_capacity(outlined.len());
        for body in outlined {
            if cancel.is_cancelled() {
                return Err(crate::backend::compile_cancel::cancelled_error());
            }
            let code = if program.sir.eval_comb.len() == 1 && body == program.sir.eval_comb[0] {
                None
            } else {
                Some(Arc::new(HotspotCode::compile(
                    &[body],
                    program.layout(),
                    options,
                    cancel,
                )?))
            };
            callees.push(code);
        }
        Ok(Self {
            program,
            budgeted,
            allocated,
            callees,
        })
    }
}

#[cfg(hotspot_jit)]
pub(super) fn hotspot_pipeline_error(source: emit::ChainedEmitError) -> SimulatorError {
    if matches!(source, emit::ChainedEmitError::Cancelled) {
        codegen_err(CodegenError::Cancelled)
    } else {
        codegen_err(CodegenError::NativePipeline { source })
    }
}

/// An independently owned complete entry using the program's existing state ABI.
#[cfg(hotspot_jit)]
pub(crate) struct HotspotCode {
    pub(super) code: jit_mem::JitCode,
    // Keep every linked mapping and its immutable address table alive until
    // the caller is retired. Nested code uses a disjoint compiler arena.
    pub(super) callees: Vec<Arc<HotspotCode>>,
    pub(super) extern_functions: Option<crate::dpi::ExternFunctionTable>,
    pub(crate) required_state_size: usize,
    pub(crate) code_size: usize,
}

#[cfg(hotspot_jit)]
impl HotspotCode {
    pub(crate) fn compile(
        units: &[crate::ir::ExecutionUnit<RegionedAbsoluteAddr>],
        layout: &MemoryLayout,
        options: &SimulatorOptions,
        cancel: &CompileCancel,
    ) -> Result<Self, SimulatorError> {
        Self::validate_units(units)?;
        let mut native = options.x86_options.clone();
        native.baseline = false;
        native.native_tick_loop = false;
        let refs = units.iter().collect::<Vec<_>>();
        let compiled = compile_unit_refs(
            &refs,
            layout,
            options.four_state,
            "hotspot",
            NativeMergeKind::Partial,
            None,
            &native,
            false,
            &options.optimize_options.diagnostics,
            Some(cancel),
        )?;
        Self::from_compiled(compiled, native.diagnostics.perf_map)
    }

    pub(super) fn validate_units(
        units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    ) -> Result<(), SimulatorError> {
        // Calls need a linked function table and disjoint nested scratch. Keep
        // them interpreted until incremental linking implements that contract.
        if units
            .iter()
            .flat_map(|unit| unit.blocks.values())
            .any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|inst| matches!(inst, SIRInstruction::Call { .. }))
            })
        {
            return Err(codegen_message(
                "hotspot entry requires an unlinked function",
            ));
        }
        Ok(())
    }

    pub(super) fn from_compiled(
        compiled: CompiledNativeFunction,
        perf_map: bool,
    ) -> Result<Self, SimulatorError> {
        let code_size = compiled.code.len();
        let code = jit_mem::JitCode::new_named_with_symbols_profiled(
            &compiled.code,
            "celox_hotspot",
            &compiled.symbols,
            perf_map,
        )
        .map_err(|error| codegen_message(format!("hotspot executable allocation: {error}")))?;
        Ok(Self {
            code,
            callees: Vec::new(),
            extern_functions: None,
            required_state_size: compiled.required_state_size,
            code_size,
        })
    }

    pub(crate) fn compile_scheduler(
        prepared: PreparedHotspotScheduler,
        comb: Arc<HotspotCode>,
        options: &SimulatorOptions,
        cancel: &CompileCancel,
    ) -> Result<Self, SimulatorError> {
        let PreparedHotspotScheduler {
            program,
            allocated,
            callees: prepared_callees,
            ..
        } = prepared;
        let mut callees = Vec::with_capacity(prepared_callees.len());
        let mut additional_bytes = 0usize;
        for prepared in prepared_callees {
            if cancel.is_cancelled() {
                return Err(crate::backend::compile_cancel::cancelled_error());
            }
            let code = if let Some(code) = prepared {
                additional_bytes = additional_bytes.saturating_add(code.code_size);
                code
            } else {
                Arc::clone(&comb)
            };
            callees.push(code);
        }
        let mut caller_options = options.clone();
        caller_options.x86_options = caller_options
            .x86_options
            .with_native_callees(callees.iter().map(|code| code.required_state_size));
        let emitted = allocated
            .emit(caller_options.x86_options.arena_base, None, || {
                cancel.is_cancelled()
            })
            .map_err(hotspot_pipeline_error)?;
        let symbols = perf_symbols_for_emit_result("hotspot", &emitted);
        let mut caller = Self::from_compiled(
            CompiledNativeFunction {
                code: emitted.code,
                symbols,
                trace: None,
                required_state_size: emitted.required_state_size as usize,
                #[cfg(target_arch = "x86_64")]
                required_native_features: emitted.required_image_features,
                #[cfg(target_arch = "aarch64")]
                required_native_features: 0,
            },
            caller_options.x86_options.diagnostics.perf_map,
        )?;
        let mut table = options
            .dpi
            .resolve(&program.runtime().runtime_schema.extern_functions)?;
        table.append_native_kernels(callees.iter().map(|code| code.code.fn_ptr as usize));
        caller.code_size = caller.code_size.saturating_add(additional_bytes);
        caller.callees = callees;
        caller.extern_functions = Some(table);
        Ok(caller)
    }

    /// The entry was emitted in this process for the caller's immutable layout.
    pub(crate) fn run(&self, memory: &mut [u64]) -> Result<(), SimulatorErrorCode> {
        if memory.len().saturating_mul(8) < self.required_state_size {
            return Err(SimulatorErrorCode::InternalError);
        }
        // SAFETY: the image is exclusively borrowed and its header is live.
        unsafe { self.run_raw(memory.as_mut_ptr().cast()) }
    }

    pub(super) unsafe fn run_raw(&self, state: *mut u8) -> Result<(), SimulatorErrorCode> {
        // Detached invocations use raw state so no Rust slice remains live
        // across their host callbacks. This mapping owns the immutable table.
        let header = unsafe {
            state
                .add(celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET)
                .cast::<u64>()
        };
        let previous = unsafe { header.read_unaligned() };
        if let Some(table) = &self.extern_functions {
            unsafe { header.write_unaligned(table.as_ptr() as u64) };
        }
        let result = unsafe { NativeBackend::call_raw(state, self.code.fn_ptr) };
        unsafe { header.write_unaligned(previous) };
        result
    }
}

#[cfg(hotspot_jit)]
impl celox_runtime::backend::NativeProcessCode for HotspotCode {
    unsafe fn invoke(
        &self,
        _index: usize,
        state: *mut u8,
        size: usize,
    ) -> Result<(), SimulatorErrorCode> {
        if size < self.required_state_size {
            return Err(SimulatorErrorCode::InternalError);
        }
        // SAFETY: the caller retains the image and header buffers throughout.
        unsafe { self.run_raw(state) }
    }
}
