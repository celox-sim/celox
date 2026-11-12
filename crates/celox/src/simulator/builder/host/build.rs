//! Building a [`Simulator`] from a configured [`SimulatorBuilder`]: the compile pipeline of each backend.

use super::*;

impl<'a> SimulatorBuilder<'a, Simulator> {
    pub fn new(code: &'a str, top: &'a str) -> Self {
        Self {
            sources: vec![(code, Path::new(""))],
            sv_sources: Vec::new(),
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            clock_type: None,
            reset_type: None,
            param_overrides: Vec::new(),
            sv_param_literals: Vec::new(),
            live_signals: Vec::new(),
            injected_components: Default::default(),
            frontend_artifact: None,
            native_file_calls: false,
            value_change_points: true,
            _marker: std::marker::PhantomData,
        }
    }

    pub fn from_sources(sources: Vec<(&'a str, &'a Path)>, top: &'a str) -> Self {
        Self {
            sources,
            sv_sources: Vec::new(),
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            clock_type: None,
            reset_type: None,
            param_overrides: Vec::new(),
            sv_param_literals: Vec::new(),
            live_signals: Vec::new(),
            injected_components: Default::default(),
            frontend_artifact: None,
            native_file_calls: false,
            value_change_points: true,
            _marker: std::marker::PhantomData,
        }
    }

    pub fn from_sv_sources(sources: Vec<(&'a str, &'a Path)>, top: &'a str) -> Self {
        Self {
            sources: Vec::new(),
            sv_sources: sources,
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            clock_type: None,
            reset_type: None,
            param_overrides: Vec::new(),
            sv_param_literals: Vec::new(),
            live_signals: Vec::new(),
            injected_components: Default::default(),
            frontend_artifact: None,
            native_file_calls: false,
            value_change_points: true,
            _marker: std::marker::PhantomData,
        }
    }

    /// Combine native Veryl and SV sources, with a top in either language.
    pub fn from_mixed_sources(
        sources: Vec<(&'a str, &'a Path)>,
        sv_sources: Vec<(&'a str, &'a Path)>,
        top: &'a str,
    ) -> Self {
        Self {
            sources,
            sv_sources,
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            clock_type: None,
            reset_type: None,
            param_overrides: Vec::new(),
            sv_param_literals: Vec::new(),
            live_signals: Vec::new(),
            injected_components: Default::default(),
            frontend_artifact: None,
            native_file_calls: false,
            value_change_points: true,
            _marker: std::marker::PhantomData,
        }
    }

    /// Low-level adapter hook for a source-independent frontend artifact.
    ///
    /// Frontend crates should wrap this with an artifact-specific public API.
    pub fn from_frontend(
        artifact: celox_frontend_sdk::FrontendArtifact,
    ) -> SimulatorBuilder<'static, Simulator> {
        SimulatorBuilder {
            sources: Vec::new(),
            sv_sources: Vec::new(),
            top: "",
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            clock_type: None,
            reset_type: None,
            param_overrides: Vec::new(),
            sv_param_literals: Vec::new(),
            live_signals: Vec::new(),
            injected_components: Default::default(),
            frontend_artifact: Some(artifact),
            native_file_calls: false,
            value_change_points: true,
            _marker: std::marker::PhantomData,
        }
    }

    /// Low-level adapter hook for a Veryl native testbench whose
    /// `$sv::Module` instances resolve from an external frontend artifact.
    pub fn from_frontend_with_testbench(
        artifact: celox_frontend_sdk::FrontendArtifact,
        sources: Vec<(&'a str, &'a Path)>,
        top: &'a str,
    ) -> Self {
        Self {
            sources,
            sv_sources: Vec::new(),
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            clock_type: None,
            reset_type: None,
            param_overrides: Vec::new(),
            sv_param_literals: Vec::new(),
            live_signals: Vec::new(),
            injected_components: Default::default(),
            frontend_artifact: Some(artifact),
            native_file_calls: false,
            value_change_points: true,
            _marker: std::marker::PhantomData,
        }
    }
}

/// Drop sampled copies no remaining unit reads. A copy's readers are
/// the processes that sampled the value; when the frontend dropped
/// those (a two-state monitor that can never fail, say), refreshing
/// the copy at every timestamp is pure overhead. Copies a host root
/// can observe stay.
pub(super) fn prune_unread_sample_shadows(program: &mut crate::ir::LaidOutProgram) {
    use crate::HashSet;
    let schema = &program.runtime().runtime_schema;
    if schema.sample_shadows.is_empty() {
        return;
    }
    let copies: HashSet<_> = schema
        .sample_shadows
        .iter()
        .map(|&(_, copy)| copy)
        .collect();
    let mut reads = HashSet::default();
    // The frontend lists every sampled copy among the testbench
    // read roots because the host refresh writes them; that
    // bookkeeping is not a read. A copy counts only when a unit
    // or a genuinely host-observable root can read it.
    reads.extend(
        schema
            .testbench_read_roots
            .iter()
            .filter(|root| !copies.contains(root))
            .copied(),
    );
    reads.extend(schema.live_signals.iter().copied());
    reads.extend(
        schema
            .comb_observers
            .iter()
            .flat_map(|observer| observer.sensitivity.iter().map(|atom| atom.id)),
    );
    reads.extend(schema.watches.iter().map(|watch| watch.signal));
    let sir = &program.sir;
    let units = sir
        .eval_comb
        .iter()
        .chain(sir.eval_apply_ffs.values().flatten())
        .chain(sir.eval_comb_apply_ffs.values().flatten())
        .chain(sir.eval_only_ffs.values().flatten())
        .chain(sir.apply_ffs.values().flatten())
        .chain(sir.processes.iter())
        .chain(sir.functions.iter().map(|function| &function.unit));
    // A call of a process kernel or a host call reports
    // `Everything`, but every callee unit is in the scanned set and
    // host calls address source-visible objects; a sampled copy is
    // internal state only a scanned unit can read.
    let mut scan = |unit: &celox_sir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>| {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                use celox_sir::StateAccess;
                match instruction.state_reads() {
                    StateAccess::None | StateAccess::Everything => {}
                    StateAccess::Access(access) => {
                        reads.insert(access.addr.absolute_addr());
                    }
                    StateAccess::Objects(objects) => {
                        reads.extend(objects.iter().map(|addr| addr.absolute_addr()));
                    }
                }
            }
        }
    };
    for unit in units {
        scan(unit);
    }
    if let Some(parallel) = &sir.parallel {
        for lane in parallel.units() {
            scan(&lane.unit);
        }
    }
    // A retained copy whose source is itself a sampled copy keeps
    // that source copy: the host refresh feeds the retained one.
    loop {
        let before = reads.len();
        for &(variable, copy) in &schema.sample_shadows {
            if reads.contains(&copy) && copies.contains(&variable) {
                reads.insert(variable);
            }
        }
        if reads.len() == before {
            break;
        }
    }
    program
        .runtime
        .runtime_schema
        .sample_shadows
        .retain(|(_, copy)| reads.contains(copy));
}

impl<'a, Target> SimulatorBuilder<'a, Target> {
    /// Compile SIR, finalize its state layout, and return the typed artifact
    /// along with the remaining builder state.
    /// Consumes self.
    pub(super) fn into_laid_out_program(
        self,
        layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
        defer_derived_edges: bool,
        wide_commits: bool,
    ) -> Result<
        (
            crate::ir::LaidOutProgram,
            Vec<CompilationWarning>,
            SimulatorOptions,
            Option<(std::path::PathBuf, crate::WaveformFormat)>,
            crate::InjectedComponents,
        ),
        SimulatorError,
    > {
        self.into_laid_out_program_with_trace(layout_mode, None, defer_derived_edges, wide_commits)
    }

    /// `wide_commits` asks the settle instrumentation for the wide FF
    /// commits only a native backend compiles
    /// ([`crate::settle::instrument_ff_units`]); VCD recording keeps the
    /// per-object commits it traces.
    pub(super) fn into_laid_out_program_with_trace(
        self,
        layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
        trace: Option<&mut crate::debug::CompilationTrace>,
        defer_derived_edges: bool,
        wide_commits: bool,
    ) -> Result<
        (
            crate::ir::LaidOutProgram,
            Vec<CompilationWarning>,
            SimulatorOptions,
            Option<(std::path::PathBuf, crate::WaveformFormat)>,
            crate::InjectedComponents,
        ),
        SimulatorError,
    > {
        let Some(path) = self.options.trace.settle_plan_output.clone() else {
            let mut result = self.into_laid_out_program_traced(
                layout_mode,
                trace,
                defer_derived_edges,
                wide_commits,
            );
            if let Ok((program, ..)) = &mut result {
                prune_unread_sample_shadows(program);
            }
            return result;
        };
        let mut local = crate::debug::CompilationTrace::default();
        let trace = trace.unwrap_or(&mut local);
        let mut result = self.into_laid_out_program_traced(
            layout_mode,
            Some(&mut *trace),
            defer_derived_edges,
            wide_commits,
        );
        if let Ok((program, ..)) = &mut result {
            prune_unread_sample_shadows(program);
        }
        write_settle_plan_output(Some(&path), trace);
        result
    }

    pub(super) fn into_laid_out_program_traced(
        mut self,
        layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
        trace: Option<&mut crate::debug::CompilationTrace>,
        defer_derived_edges: bool,
        wide_commits: bool,
    ) -> Result<
        (
            crate::ir::LaidOutProgram,
            Vec<CompilationWarning>,
            SimulatorOptions,
            Option<(std::path::PathBuf, crate::WaveformFormat)>,
            crate::InjectedComponents,
        ),
        SimulatorError,
    > {
        self.options.diagnostic_policy = self
            .options
            .diagnostic_policy
            .clone()
            .with_inferred_latches(self.options.allow_inferred_latches);
        // VCD descriptors currently describe packed whole objects. Apply
        // the same layout rule to every factory, including native images.
        let layout_mode = if self.vcd_path.is_some() {
            crate::backend::memory_layout::MemoryLayoutMode::Packed
        } else {
            layout_mode
        };
        self.enforce_native_force_optimizer();
        self.declare_unobserved_nets();
        let phase_timing = self.options.diagnostics.phase_timing;
        let compile_start = phase_timing.then(crate::timing::now);
        let injected_manifests = self.injected_components.manifests();
        let compiled = if !self.sv_param_literals.is_empty() && self.frontend_artifact.is_some() {
            Err(crate::CodegenError::message(
                "parameter literals require pure SystemVerilog sources",
            )
            .into())
        } else if let Some(artifact) = &self.frontend_artifact {
            if self.sources.is_empty() {
                compile_frontend_to_sir_with_layout_mode(
                    artifact,
                    &self.ignored_loops,
                    &self.true_loops,
                    self.options.four_state,
                    &self.options.trace,
                    trace,
                    &self.options.optimize_options,
                    &self.options.diagnostics,
                    layout_mode,
                )
            } else {
                compile_frontend_testbench_to_sir_with_layout_mode(
                    artifact,
                    &self.sources,
                    self.top,
                    &self.ignored_loops,
                    &self.true_loops,
                    self.options.four_state,
                    &self.options.trace,
                    trace,
                    self.metadata,
                    self.clock_type,
                    self.reset_type,
                    &self.options.optimize_options,
                    &self.options.diagnostics,
                    &self.options.diagnostic_policy,
                    self.options.sv_timescale,
                    self.options.legacy_empty_reductions,
                    &injected_manifests,
                    layout_mode,
                    !self.options.native_force_support,
                    self.options.allow_always_ff_function_effects,
                    self.options.allow_inferred_latches,
                )
            }
        } else {
            compile_hdl_to_sir_with_layout_mode(
                &self.sources,
                &self.sv_sources,
                self.top,
                &self.ignored_loops,
                &self.true_loops,
                self.options.four_state,
                &self.options.trace,
                trace,
                self.metadata,
                self.clock_type,
                self.reset_type,
                &self.param_overrides,
                &self.options.optimize_options,
                &self.options.diagnostics,
                &self.options.diagnostic_policy,
                self.options.sv_timescale,
                self.options.legacy_empty_reductions,
                &injected_manifests,
                layout_mode,
                !self.options.native_force_support,
                self.options.allow_always_ff_function_effects,
                self.options.allow_inferred_latches,
                &self.options.signal_access.force_signals,
                &self.sv_param_literals,
            )
        };
        #[cfg_attr(not(test), allow(unused_mut))]
        let (mut program, mut warnings) = compiled
            .map_err(|error| error.apply_diagnostic_policy(&self.options.diagnostic_policy))?;
        #[cfg(test)]
        if let Some(transform) = self.options.sir_transform {
            transform(&mut program.sir);
        }
        self.options.diagnostic_policy.apply(&mut warnings)?;
        if let Some(start) = compile_start {
            tracing::debug!("[phase-timing] compile_to_sir: {:?}", start.elapsed());
        }

        // A source request can originate in an include or mixed-language
        // child. Decide from the elaborated schema before physical layout.
        let vcd_recording = self.vcd_path.is_some()
            || program
                .runtime_schema
                .host_request_sites
                .iter()
                .any(|site| site.kind == celox_design::HostRequestKind::Vcd);
        let layout_mode = if vcd_recording {
            crate::backend::memory_layout::MemoryLayoutMode::Packed
        } else {
            layout_mode
        };
        // Build memory layout (consumes semantic layout requirements).
        let layout_start = phase_timing.then(crate::timing::now);
        // The wide commits read state words no analysis can name: the
        // FF units get them after the dead-store elimination.
        let wide_commits = wide_commits && !vcd_recording;
        let mut laid_out =
            program.into_laid_out_with_options(self.options.four_state, layout_mode, wide_commits);
        if vcd_recording {
            laid_out.enable_vcd_tracking();
        }
        if let Some(start) = layout_start {
            tracing::debug!("[phase-timing] build_layout: {:?}", start.elapsed());
        }

        validate_signal_access(&laid_out, &self.options.signal_access)?;
        validate_value_change_signals(&laid_out, &self.options.optimize_options)?;
        if self.options.dead_store_policy != DeadStorePolicy::Off {
            let dse_start = phase_timing.then(crate::timing::now);
            run_dead_store_elimination(&mut laid_out, &self.live_signals, &self.options);
            if let Some(start) = dse_start {
                tracing::debug!(
                    "[phase-timing] dead_store_elimination: {:?}",
                    start.elapsed()
                );
            }
        }
        #[cfg(feature = "host-runtime")]
        crate::value_change::lower(
            &mut laid_out,
            self.native_file_calls && self.value_change_points,
        );

        if wide_commits {
            // The units outside the settle were left uninstrumented:
            // mark from the final partition bodies.
            laid_out.prune_stale_marks();
            crate::settle::instrument_processes_from(&mut laid_out, 0, false);
            crate::settle::instrument_ff_units(&mut laid_out, true);
        }

        // Units generated from here on write state the settle may read.
        let first_generated_process = laid_out.sir.processes.len();
        // A timed simulation runs the slots of kernels that only delay
        // in a compiled scheduler, generated from the final units.
        #[cfg(feature = "host-runtime")]
        if (self.options.emit_triggers || laid_out.runtime.testbench.is_some())
            && self.options.scheduler != crate::SchedulerMode::Reference
        {
            if self.native_file_calls {
                crate::scheduler::enable_native_services(&mut laid_out);
            }
            if let Some(scheduler) = laid_out.runtime.runtime_schema.scheduler.as_mut() {
                scheduler.update_points = self.options.observable_updates;
            }
            let plan_start = self
                .options
                .diagnostics
                .phase_timing
                .then(crate::timing::now);
            crate::scheduler::attach(
                &mut laid_out,
                !self.options.native_force_support
                    && self.options.signal_access.force_signals.is_empty(),
            );
            if let Some(start) = plan_start {
                tracing::debug!("[phase-timing] scheduler_plan: {:?}", start.elapsed());
            }
        }
        #[cfg(feature = "host-runtime")]
        if self
            .options
            .optimize_options
            .is_enabled(crate::optimizer::SirPass::DerivedEdgeUnits)
        {
            if defer_derived_edges {
                crate::derived_edge::attach_deferred(&mut laid_out, self.options.host_edges);
            } else {
                crate::derived_edge::attach(&mut laid_out, self.options.host_edges);
            }
        }
        #[cfg(all(test, feature = "cranelift"))]
        if let Some(sink) = &self.options.derived_edge_units {
            crate::derived_edge::attach_test_units(&mut laid_out, sink);
        }
        crate::settle::instrument_processes_from(&mut laid_out, first_generated_process, false);
        laid_out.set_settle_check(self.options.settle_check);

        // Link DPI-C imports before code generation, so a missing C
        // function fails the build early; the backends reuse the result.
        self.options
            .dpi
            .resolve_ahead(&laid_out.runtime().runtime_schema.extern_functions)
            .map_err(|error| {
                SimulatorError::from(error).with_warnings(std::mem::take(&mut warnings))
            })?;

        Ok((
            laid_out,
            warnings,
            self.options,
            self.vcd_path,
            self.injected_components,
        ))
    }
}

impl<'a> SimulatorBuilder<'a, Simulator> {
    /// Compiles the Veryl source and constructs the simulator.
    /// Uses a custom native backend when its generated code matches the
    /// compilation target, and Cranelift for host-side cross-codegen builds.
    pub fn build(self) -> Result<Simulator<crate::DefaultBackend>, SimulatorError> {
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        {
            self.build_native()
        }
        #[cfg(not(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        )))]
        {
            self.build_cranelift()
        }
    }

    /// Compiles using the Cranelift JIT backend.
    #[cfg(feature = "cranelift")]
    pub fn build_cranelift(self) -> Result<Simulator<JitBackend>, SimulatorError> {
        let phase_timing = self.options.diagnostics.phase_timing;
        let phase_start = phase_timing.then(crate::timing::now);

        let (laid_out, mut warnings, options, vcd_path, injected_components) = self
            .into_laid_out_program(
                crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
                false,
                false,
            )?;
        validate_untimed_processes(laid_out.runtime())?;

        if let Some(s) = phase_start {
            tracing::debug!(
                "[phase-timing] compile_and_layout (total): {:?}",
                s.elapsed()
            );
        }

        let jit_start = phase_timing.then(crate::timing::now);
        let mut trace = crate::debug::CompilationTrace::default();
        let wants_codegen_trace = options.trace.pre_optimized_clif
            || options.trace.post_optimized_clif
            || options.trace.native;
        let backend = JitBackend::new(
            &laid_out,
            &options,
            wants_codegen_trace.then_some(&mut trace),
        )
        .map_err(|error| error.with_warnings(std::mem::take(&mut warnings)))?;
        if options.trace.output_to_stdout {
            trace.print();
        }
        if let Some(s) = jit_start {
            tracing::debug!("[phase-timing] jit_backend: {:?}", s.elapsed());
        }

        let mut sim =
            Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
        sim.components.set_injected(injected_components);
        sim.diagnostics = options.diagnostics.clone();
        sim.set_plusargs(options.plusargs.clone());
        sim.set_file_io(celox_runtime::file_io::FileIo::new(
            options.file_system.create(),
        ));
        if let Some((path, format)) = vcd_path {
            let descs = sim.build_vcd_descs(options.four_state);
            let vcd_writer = crate::WaveformWriter::new_with_time_precision(
                path,
                format,
                &descs,
                sim.program.design.time_precision,
            )
            .map_err(|_| {
                SimulatorError::from(crate::RuntimeErrorCode::InternalError)
                    .with_warnings(std::mem::take(&mut sim.warnings))
            })?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.apply_initial_values();
        sim.modify(|_| {}).map_err(|error| {
            SimulatorError::from(error).with_warnings(std::mem::take(&mut sim.warnings))
        })?;
        Ok(sim)
    }

    /// Compiles SIR, finalizes the state layout, and returns a simulator
    /// whose execution units run on the Tier-0 SIR interpreter instead of
    /// generated machine code.
    ///
    /// This is the target of the `all_backends!` `interp` test arm. No
    /// code generation happens on this path: the simulator is ready as
    /// soon as the state layout is finalized, and every execution unit is
    /// driven by [`crate::interpreter::execute_unit`] against the same
    /// memory image ABI the compiled backends use.
    pub fn build_interpreter(
        self,
    ) -> Result<Simulator<crate::backend::InterpBackend>, SimulatorError> {
        let phase_timing = self.options.diagnostics.phase_timing;
        let phase_start = phase_timing.then(crate::timing::now);

        let (laid_out, mut warnings, options, vcd_path, injected_components) = self
            .into_laid_out_program(
                crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
                false,
                false,
            )?;
        validate_untimed_processes(laid_out.runtime())?;

        if let Some(s) = phase_start {
            tracing::debug!(
                "[phase-timing] compile_and_layout (total): {:?}",
                s.elapsed()
            );
        }

        let backend = crate::backend::InterpBackend::new(&laid_out, &options)
            .map_err(|error| error.with_warnings(std::mem::take(&mut warnings)))?;

        let mut sim =
            Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
        sim.components.set_injected(injected_components);
        sim.diagnostics = options.diagnostics.clone();
        sim.set_plusargs(options.plusargs.clone());
        sim.set_file_io(celox_runtime::file_io::FileIo::new(
            options.file_system.create(),
        ));
        if let Some((path, format)) = vcd_path {
            let descs = sim.build_vcd_descs(options.four_state);
            let vcd_writer = crate::WaveformWriter::new_with_time_precision(
                path,
                format,
                &descs,
                sim.program.design.time_precision,
            )
            .map_err(|_| {
                SimulatorError::from(crate::RuntimeErrorCode::InternalError)
                    .with_warnings(std::mem::take(&mut sim.warnings))
            })?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.apply_initial_values();
        sim.modify(|_| {}).map_err(|error| {
            SimulatorError::from(error).with_warnings(std::mem::take(&mut sim.warnings))
        })?;
        Ok(sim)
    }

    /// Compiles SIR, finalizes the state layout, and returns a simulator
    /// that starts executing on the Tier-0 interpreter immediately while
    /// the host's default compiled tier (native where available,
    /// Cranelift otherwise) is compiled in the background.
    ///
    /// The first scheduler safe point after background compilation
    /// completes adopts the compiled code and moves the live memory image
    /// across without translation. If background compilation fails the
    /// simulator keeps running on the interpreter.
    pub fn build_tiered(self) -> Result<Simulator<crate::backend::TieredBackend>, SimulatorError> {
        let phase_timing = self.options.diagnostics.phase_timing;
        let phase_start = phase_timing.then(crate::timing::now);

        let (laid_out, warnings, options, vcd_path, injected_components) = {
            let vcd_recording = self.vcd_path.is_some();
            // As in the timed `build_tiered`: only a native image runs
            // derived-edge units, and it expands deferred ones itself.
            // Wide FF commits match the native build where the
            // compiled tier is a native image, which alone compiles
            // them; the interpreter runs the same units through their
            // typed shadow commits. A Cranelift tier gets none.
            self.into_laid_out_program(
                crate::backend::tiered::tiered_layout_mode(vcd_recording),
                crate::backend::tiered::native_is_default_target(),
                crate::backend::tiered::native_is_default_target(),
            )?
        };
        validate_untimed_processes(laid_out.runtime())?;

        if let Some(s) = phase_start {
            tracing::debug!(
                "[phase-timing] compile_and_layout (total): {:?}",
                s.elapsed()
            );
        }

        let laid_out = std::sync::Arc::new(laid_out);
        let backend = crate::backend::TieredBackend::new_shared(&laid_out, &options);
        // A backend that kept the program leaves only its runtime metadata
        // to copy.
        let runtime = match std::sync::Arc::try_unwrap(laid_out) {
            Ok(laid_out) => laid_out.into_runtime(),
            Err(shared) => shared.runtime().clone(),
        };

        let mut sim = Simulator::with_backend_and_program(backend, runtime, warnings);
        sim.components.set_injected(injected_components);
        sim.diagnostics = options.diagnostics.clone();
        sim.set_plusargs(options.plusargs.clone());
        sim.set_file_io(celox_runtime::file_io::FileIo::new(
            options.file_system.create(),
        ));
        if let Some((path, format)) = vcd_path {
            let descs = sim.build_vcd_descs(options.four_state);
            let vcd_writer = crate::WaveformWriter::new_with_time_precision(
                path,
                format,
                &descs,
                sim.program.design.time_precision,
            )
            .map_err(|_| {
                SimulatorError::from(crate::RuntimeErrorCode::InternalError)
                    .with_warnings(std::mem::take(&mut sim.warnings))
            })?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.apply_initial_values();
        sim.modify(|_| {}).map_err(|error| {
            SimulatorError::from(error).with_warnings(std::mem::take(&mut sim.warnings))
        })?;
        Ok(sim)
    }

    /// Build a tiered simulator with a caller-provided compilation step.
    ///
    /// Used by tests to make promotion timing deterministic; `compile`
    /// runs on the background worker thread.
    /// The optimizer options this builder will compile with.
    #[cfg(test)]
    pub(crate) fn optimize_options_for_test(&self) -> &crate::optimizer::OptimizeOptions {
        &self.options.optimize_options
    }

    #[cfg(test)]
    pub(crate) fn build_tiered_with_compiler<F>(
        self,
        compile: F,
    ) -> Result<Simulator<crate::backend::TieredBackend>, SimulatorError>
    where
        F: FnOnce(
                &crate::ir::LaidOutProgram,
                &SimulatorOptions,
                &crate::backend::compile_cancel::CompileCancel,
            ) -> Result<crate::backend::tiered::CompiledCode, SimulatorError>
            + Send
            + 'static,
    {
        let phase_timing = self.options.diagnostics.phase_timing;
        let phase_start = phase_timing.then(crate::timing::now);

        let (laid_out, warnings, options, vcd_path, injected_components) = {
            let vcd_recording = self.vcd_path.is_some();
            // As in the timed `build_tiered`: only a native image runs
            // derived-edge units, and it expands deferred ones itself.
            // Wide FF commits match the native build where the
            // compiled tier is a native image, which alone compiles
            // them; the interpreter runs the same units through their
            // typed shadow commits. A Cranelift tier gets none.
            self.into_laid_out_program(
                crate::backend::tiered::tiered_layout_mode(vcd_recording),
                crate::backend::tiered::native_is_default_target(),
                crate::backend::tiered::native_is_default_target(),
            )?
        };
        validate_untimed_processes(laid_out.runtime())?;

        if let Some(s) = phase_start {
            tracing::debug!(
                "[phase-timing] compile_and_layout (total): {:?}",
                s.elapsed()
            );
        }

        let backend = crate::backend::TieredBackend::with_compiler(&laid_out, &options, compile);

        let mut sim =
            Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
        sim.components.set_injected(injected_components);
        sim.diagnostics = options.diagnostics.clone();
        sim.set_plusargs(options.plusargs.clone());
        sim.set_file_io(celox_runtime::file_io::FileIo::new(
            options.file_system.create(),
        ));
        if let Some((path, format)) = vcd_path {
            let descs = sim.build_vcd_descs(options.four_state);
            let vcd_writer = crate::WaveformWriter::new_with_time_precision(
                path,
                format,
                &descs,
                sim.program.design.time_precision,
            )
            .map_err(|_| {
                SimulatorError::from(crate::RuntimeErrorCode::InternalError)
                    .with_warnings(std::mem::take(&mut sim.warnings))
            })?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.apply_initial_values();
        sim.modify(|_| {}).map_err(|error| {
            SimulatorError::from(error).with_warnings(std::mem::take(&mut sim.warnings))
        })?;
        Ok(sim)
    }

    /// Compiles using the selected native code-generation architecture.
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub fn compile_native(self) -> Result<NativeCompilation, SimulatorError> {
        let phase_timing = self.options.diagnostics.phase_timing;
        let sir_start = phase_timing.then(crate::timing::now);
        let (laid_out, mut warnings, options, vcd_path, injected_components) = self
            .into_laid_out_program(
                crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
                true,
                true,
            )?;
        validate_untimed_processes(laid_out.runtime())?;
        if let Some(start) = sir_start {
            tracing::debug!(
                "[phase-timing] into_laid_out_program total: {:?}",
                start.elapsed()
            );
        }
        let backend_start = phase_timing.then(crate::timing::now);
        let image = crate::backend::native::NativeBackend::compile_image(&laid_out, &options)
            .map_err(|error| error.with_warnings(std::mem::take(&mut warnings)))?;
        if let Some(start) = backend_start {
            tracing::debug!("[phase-timing] native_codegen: {:?}", start.elapsed());
        }
        Ok(NativeCompilation {
            image,
            program: laid_out,
            warnings,
            options,
            vcd_path,
            injected_components,
            _target: std::marker::PhantomData,
        })
    }

    /// Compiles using the selected native code-generation architecture and
    /// returns the trace captured from the same native compilation.
    ///
    /// Unlike [`Self::build_with_trace`], this method never loads or runs
    /// the generated machine code, so it is available during cross-codegen.
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub fn compile_native_with_trace(
        self,
    ) -> Result<(NativeCompilation, crate::debug::CompilationTrace), SimulatorError> {
        let phase_timing = self.options.diagnostics.phase_timing;
        let sir_start = phase_timing.then(crate::timing::now);
        let mut trace = crate::debug::CompilationTrace::default();
        let (laid_out, mut warnings, options, vcd_path, injected_components) = self
            .into_laid_out_program_with_trace(
                crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
                Some(&mut trace),
                true,
                true,
            )?;
        validate_untimed_processes(laid_out.runtime())?;
        if let Some(start) = sir_start {
            tracing::debug!(
                "[phase-timing] into_laid_out_program total: {:?}",
                start.elapsed()
            );
        }
        let backend_start = phase_timing.then(crate::timing::now);
        let (image, native_trace) =
            crate::backend::native::NativeBackend::compile_image_with_codegen_trace(
                &laid_out, &options,
            )
            .map_err(|error| error.with_warnings(std::mem::take(&mut warnings)))?;
        if let Some(start) = backend_start {
            tracing::debug!("[phase-timing] native_codegen: {:?}", start.elapsed());
        }
        trace.native_optimized_sir = Some(native_trace.optimized_sir);
        trace.mir = Some(native_trace.mir);
        trace.reactive_event_graph = Some(native_trace.reactive_graph);
        trace.native_state_layout = Some(native_trace.state_layout);
        if options.trace.output_to_stdout {
            trace.print();
        }
        Ok((
            NativeCompilation {
                image,
                program: laid_out,
                warnings,
                options,
                vcd_path,
                injected_components,
                _target: std::marker::PhantomData,
            },
            trace,
        ))
    }

    /// Compiles for an x86-64 target and initializes the simulator.
    /// This includes Cargo cross-builds whose target is x86-64.
    #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
    pub fn build_x86_64(
        self,
    ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
        self.compile_native()?.initialize()
    }

    /// Compiles for an AArch64 target and initializes the simulator.
    /// This includes Cargo cross-builds whose target is AArch64.
    #[cfg(all(target_arch = "aarch64", not(feature = "x86_64-codegen")))]
    pub fn build_arm64(
        self,
    ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
        self.compile_native()?.initialize()
    }

    /// Compiles using the native backend selected for the compilation target.
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    pub fn build_native(
        self,
    ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
        #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
        {
            self.build_x86_64()
        }
        #[cfg(all(target_arch = "aarch64", not(feature = "x86_64-codegen")))]
        {
            self.build_arm64()
        }
    }

    /// Load a previously generated native image without running source
    /// parsing, SIR construction, layout, or native machine-code
    /// generation here.
    ///
    /// This is the execution-side half of the host-codegen/QEMU workflow:
    /// source-independent runtime metadata and the semantic testbench are
    /// restored from `image`, while the target machine code is taken
    /// entirely from it.
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    #[allow(unreachable_code)]
    pub fn build_native_from_image(
        self,
        image: crate::backend::native::NativeProgramImage,
    ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
        let mut options = self.options;
        options.four_state = image.four_state();
        let vcd_path = self.vcd_path;
        let injected_components = self.injected_components;

        #[cfg(any(
            all(feature = "arm64-codegen", not(target_arch = "aarch64")),
            all(feature = "x86_64-codegen", not(target_arch = "x86_64"))
        ))]
        {
            let _ = (image, options, vcd_path, injected_components);
            return Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError));
        }

        let program = image.runtime_program();
        // Safety: the caller is explicitly loading a trusted native-image
        // artifact, and `from_image` validates its structure first.
        let backend = unsafe {
            crate::backend::native::NativeBackend::from_image_with_dpi(image, &options.dpi)?
        };
        initialize_native_backend(
            backend,
            program,
            Vec::new(),
            options,
            vcd_path,
            injected_components,
        )
    }

    /// Compiles using the Wasmtime WASM backend.
    #[cfg(feature = "wasm-runtime")]
    pub fn build_wasm(
        self,
    ) -> Result<Simulator<crate::backend::wasm_runtime::WasmBackend>, SimulatorError> {
        let (laid_out, mut warnings, options, vcd_path, injected_components) = self
            .into_laid_out_program(
                crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
                false,
                false,
            )?;
        validate_untimed_processes(laid_out.runtime())?;
        let backend = crate::backend::wasm_runtime::WasmBackend::new(&laid_out, &options)
            .map_err(|error| error.with_warnings(std::mem::take(&mut warnings)))?;
        let mut sim =
            Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
        sim.components.set_injected(injected_components);
        sim.diagnostics = options.diagnostics.clone();
        sim.set_plusargs(options.plusargs.clone());
        sim.set_file_io(celox_runtime::file_io::FileIo::new(
            options.file_system.create(),
        ));
        if let Some((path, format)) = vcd_path {
            let descs = sim.build_vcd_descs(options.four_state);
            let vcd_writer = crate::WaveformWriter::new_with_time_precision(
                path,
                format,
                &descs,
                sim.program.design.time_precision,
            )
            .map_err(|_| {
                SimulatorError::from(crate::RuntimeErrorCode::InternalError)
                    .with_warnings(std::mem::take(&mut sim.warnings))
            })?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.apply_initial_values();
        sim.modify(|_| {}).map_err(|error| {
            SimulatorError::from(error).with_warnings(std::mem::take(&mut sim.warnings))
        })?;
        Ok(sim)
    }

    /// Compiles and runs a native testbench (`#[test]` module).
    pub fn run_test(self) -> Result<crate::testbench::TestResult, SimulatorError> {
        run_test_with_sim(self.build()?)
    }

    /// Compiles and runs a testbench using the Cranelift JIT backend.
    #[cfg(feature = "cranelift")]
    pub fn run_test_cranelift(self) -> Result<crate::testbench::TestResult, SimulatorError> {
        run_test_with_sim(self.build_cranelift()?)
    }

    /// Compiles and runs a testbench using the custom native backend.
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    pub fn run_test_native(self) -> Result<crate::testbench::TestResult, SimulatorError> {
        run_test_with_sim(self.build_native()?)
    }

    /// Compiles and runs a native testbench, returning assertion results
    /// observed before the test finishes or stops on a fatal failure.
    pub fn run_test_detailed(self) -> Result<crate::testbench::TestResultDetailed, SimulatorError> {
        let mut sim = self.build()?;
        let testbench = crate::testbench::compile_initial_testbench(&sim).ok_or_else(|| {
            SimulatorError::new(SimulatorErrorKind::Codegen(crate::CodegenError::message(
                "no initial block found — this module is not a native testbench",
            )))
            .with_warnings(std::mem::take(&mut sim.warnings))
        })?;
        Ok(crate::testbench::run_testbench_detailed(
            &mut sim, &testbench,
        ))
    }

    /// Compiles the Veryl source and constructs the core logic simulator,
    /// while capturing compilation trace data as configured by TraceOptions.
    pub fn build_with_trace(mut self) -> crate::debug::CompilationTraceResult {
        self.options.diagnostic_policy = self
            .options
            .diagnostic_policy
            .clone()
            .with_inferred_latches(self.options.allow_inferred_latches);
        self.enforce_native_force_optimizer();
        self.declare_unobserved_nets();
        let mut trace = crate::debug::CompilationTrace::default();
        let layout_mode = crate::backend::memory_layout::MemoryLayoutMode::ElementStrided;
        let layout_mode = if self.vcd_path.is_some() {
            crate::backend::memory_layout::MemoryLayoutMode::Packed
        } else {
            layout_mode
        };
        let injected_manifests = self.injected_components.manifests();
        let program_res = if !self.sv_param_literals.is_empty() && self.frontend_artifact.is_some()
        {
            Err(crate::CodegenError::message(
                "parameter literals require pure SystemVerilog sources",
            )
            .into())
        } else if let Some(artifact) = &self.frontend_artifact {
            if self.sources.is_empty() {
                compile_frontend_to_sir_with_layout_mode(
                    artifact,
                    &self.ignored_loops,
                    &self.true_loops,
                    self.options.four_state,
                    &self.options.trace,
                    Some(&mut trace),
                    &self.options.optimize_options,
                    &self.options.diagnostics,
                    layout_mode,
                )
            } else {
                compile_frontend_testbench_to_sir_with_layout_mode(
                    artifact,
                    &self.sources,
                    self.top,
                    &self.ignored_loops,
                    &self.true_loops,
                    self.options.four_state,
                    &self.options.trace,
                    Some(&mut trace),
                    self.metadata,
                    self.clock_type,
                    self.reset_type,
                    &self.options.optimize_options,
                    &self.options.diagnostics,
                    &self.options.diagnostic_policy,
                    self.options.sv_timescale,
                    self.options.legacy_empty_reductions,
                    &injected_manifests,
                    layout_mode,
                    !self.options.native_force_support,
                    self.options.allow_always_ff_function_effects,
                    self.options.allow_inferred_latches,
                )
            }
        } else {
            compile_hdl_to_sir_with_layout_mode(
                &self.sources,
                &self.sv_sources,
                self.top,
                &self.ignored_loops,
                &self.true_loops,
                self.options.four_state,
                &self.options.trace,
                Some(&mut trace),
                self.metadata,
                self.clock_type,
                self.reset_type,
                &self.param_overrides,
                &self.options.optimize_options,
                &self.options.diagnostics,
                &self.options.diagnostic_policy,
                self.options.sv_timescale,
                self.options.legacy_empty_reductions,
                &injected_manifests,
                layout_mode,
                !self.options.native_force_support,
                self.options.allow_always_ff_function_effects,
                self.options.allow_inferred_latches,
                &self.options.signal_access.force_signals,
                &self.sv_param_literals,
            )
        };

        write_settle_plan_output(self.options.trace.settle_plan_output.as_deref(), &trace);
        let program_res = program_res
            .map_err(|error| error.apply_diagnostic_policy(&self.options.diagnostic_policy));
        let sim_res = program_res.and_then(|(program, mut warnings)| {
            self.options.diagnostic_policy.apply(&mut warnings)?;
            let vcd_recording = self.vcd_path.is_some()
                || program
                    .runtime_schema
                    .host_request_sites
                    .iter()
                    .any(|site| site.kind == celox_design::HostRequestKind::Vcd);
            let layout_mode = if vcd_recording {
                crate::backend::memory_layout::MemoryLayoutMode::Packed
            } else {
                layout_mode
            };
            let mut laid_out =
                program.into_laid_out_with_mode(self.options.four_state, layout_mode);
            validate_untimed_processes(laid_out.runtime())?;
            if vcd_recording {
                laid_out.enable_vcd_tracking();
            }

            validate_signal_access(&laid_out, &self.options.signal_access)?;
            validate_value_change_signals(&laid_out, &self.options.optimize_options)?;
            if self.options.dead_store_policy != DeadStorePolicy::Off {
                run_dead_store_elimination(&mut laid_out, &self.live_signals, &self.options);
            }
            prune_unread_sample_shadows(&mut laid_out);
            crate::value_change::lower(&mut laid_out, false);

            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            let backend =
                if self.options.trace.mir || !self.options.trace.native_profile_blocks.is_empty() {
                    let (backend, native_trace) =
                        crate::backend::native::NativeBackend::new_with_codegen_trace(
                            &laid_out,
                            &self.options,
                        )
                        .map_err(|error| error.with_warnings(std::mem::take(&mut warnings)))?;
                    trace.native_optimized_sir = Some(native_trace.optimized_sir);
                    trace.mir = Some(native_trace.mir);
                    trace.reactive_event_graph = Some(native_trace.reactive_graph);
                    trace.native_state_layout = Some(native_trace.state_layout);
                    backend
                } else {
                    crate::backend::native::NativeBackend::new(&laid_out, &self.options)
                        .map_err(|error| error.with_warnings(std::mem::take(&mut warnings)))?
                };
            #[cfg(not(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            )))]
            let backend = JitBackend::new(&laid_out, &self.options, None)
                .map_err(|error| error.with_warnings(std::mem::take(&mut warnings)))?;

            let mut sim =
                Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
            sim.components
                .set_injected(self.injected_components.clone());
            sim.diagnostics = self.options.diagnostics.clone();
            sim.set_plusargs(self.options.plusargs.clone());
            sim.set_file_io(celox_runtime::file_io::FileIo::new(
                self.options.file_system.create(),
            ));
            sim.apply_initial_values();
            sim.modify(|_| {}).map_err(|error| {
                SimulatorError::from(error).with_warnings(std::mem::take(&mut sim.warnings))
            })?;
            Ok(sim)
        });

        if self.options.trace.output_to_stdout {
            trace.print();
        }

        crate::debug::CompilationTraceResult {
            res: sim_res,
            trace,
        }
    }
}

pub(super) fn run_test_with_sim<B: crate::backend::SimBackend>(
    mut sim: Simulator<B>,
) -> Result<crate::testbench::TestResult, SimulatorError> {
    let phase_timing = sim.diagnostics.phase_timing;
    let testbench_start = phase_timing.then(crate::timing::now);
    let testbench = crate::testbench::compile_initial_testbench(&sim).ok_or_else(|| {
        SimulatorError::new(SimulatorErrorKind::Codegen(crate::CodegenError::message(
            "no initial block found — this module is not a native testbench",
        )))
        .with_warnings(std::mem::take(&mut sim.warnings))
    })?;
    let result = crate::testbench::run_testbench(&mut sim, &testbench);
    if let Some(start) = testbench_start {
        tracing::debug!("[phase-timing] testbench: {:?}", start.elapsed());
    }
    Ok(result)
}

pub(super) fn validate_value_change_signals(
    program: &LaidOutProgram,
    options: &crate::optimizer::OptimizeOptions,
) -> Result<(), SimulatorError> {
    for path in options.value_change_objects() {
        if program.hierarchical_signal_addr(path).is_none() {
            return Err(crate::CodegenError::message(format!(
                "unknown or ambiguous signal `{path}`"
            ))
            .into());
        }
    }
    Ok(())
}

pub(super) fn validate_signal_access(
    program: &LaidOutProgram,
    access: &crate::SignalAccessOptions,
) -> Result<(), SimulatorError> {
    for path in access.live_signals.iter().chain(&access.force_signals) {
        if program.hierarchical_signal_addr(path).is_none() {
            return Err(crate::CodegenError::message(format!(
                "unknown or ambiguous signal `{path}`"
            ))
            .into());
        }
    }
    Ok(())
}

/// Resolve user-specified `(instance_path, var_path)` to `AbsoluteAddr` and run DSE.
pub(super) fn run_dead_store_elimination(
    program: &mut LaidOutProgram,
    live_signals: &[(Vec<(String, usize)>, Vec<String>)],
    options: &SimulatorOptions,
) {
    use crate::HashSet;
    let mut externally_live = HashSet::default();
    if program
        .runtime_schema
        .host_request_sites
        .iter()
        .any(|site| site.kind == celox_design::HostRequestKind::Vcd)
    {
        // A runtime hierarchy selection may observe every authored object.
        externally_live.extend(
            program
                .design
                .instances()
                .flat_map(|instance| instance.state_addresses().iter().copied()),
        );
    }

    // Native testbench expressions bypass SIR and read simulator memory
    // directly. Their inputs are therefore external DSE roots just like
    // signals named with `live_signal()`.
    externally_live.extend(program.runtime_schema.testbench_read_roots.iter().copied());

    let mut listed = HashSet::default();
    for path in options
        .signal_access
        .live_signals
        .iter()
        .chain(&options.signal_access.force_signals)
        .chain(options.optimize_options.value_change_objects())
    {
        let addr = program
            .hierarchical_signal_addr(path)
            .expect("validated signal path");
        externally_live.insert(addr);
        listed.insert(addr);
    }

    // User-specified live signals
    for (inst_path, var_path) in live_signals {
        let inst_refs: Vec<(&str, usize)> =
            inst_path.iter().map(|(s, i)| (s.as_str(), *i)).collect();
        let var_refs: Vec<&str> = var_path.iter().map(|s| s.as_str()).collect();
        let addr = program.get_addr(&inst_refs, &var_refs).unwrap();
        externally_live.insert(addr);
        listed.insert(addr);
    }
    // The runtime resolves handles from the policy and the design, not
    // from what this particular run happened to keep.
    program.runtime.runtime_schema.dead_store_policy = options.dead_store_policy;
    program.runtime.runtime_schema.live_signals = listed;

    // PreserveTopPorts: auto-collect top module port addresses
    if options.dead_store_policy == DeadStorePolicy::PreserveTopPorts {
        if let Some(instance) = program.design.root_instance() {
            for address in instance.state_addresses() {
                if program.design.variable(address).unwrap().var_kind.is_port() {
                    externally_live.insert(*address);
                }
            }
        }
    }

    // PreserveAllPorts: collect port addresses from every instance
    if options.dead_store_policy == DeadStorePolicy::PreserveAllPorts {
        for instance in program.design.instances() {
            for address in instance.state_addresses() {
                if program.design.variable(address).unwrap().var_kind.is_port() {
                    externally_live.insert(*address);
                }
            }
        }
    }

    crate::optimizer::sir::optimize_rooted_comb_memory(
        program,
        &externally_live,
        options.four_state,
    );
}
