//! Building a timed [`crate::Simulation`] from a configured [`SimulatorBuilder`].

use super::*;

impl<'a> SimulatorBuilder<'a, crate::Simulation> {
    /// Build without executing declaration or combinational initialization.
    /// Use when preparing a checkpoint restore target. Without a restore,
    /// the first simulation step performs the pending initialization.
    pub fn defer_initialization(mut self, defer: bool) -> Self {
        self.options.defer_initialization = defer;
        self
    }

    pub(crate) fn new(code: &'a str, top: &'a str) -> Self {
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

    pub(crate) fn from_sources(sources: Vec<(&'a str, &'a Path)>, top: &'a str) -> Self {
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

    pub(crate) fn from_frontend(
        artifact: celox_frontend_sdk::FrontendArtifact,
    ) -> SimulatorBuilder<'static, crate::Simulation> {
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

    /// Compiles the design using the host's default backend.
    pub fn build(self) -> Result<crate::Simulation, SimulatorError> {
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

    /// Compiles a timed simulation using the Cranelift JIT backend.
    #[cfg(feature = "cranelift")]
    pub fn build_cranelift(self) -> Result<crate::Simulation<JitBackend>, SimulatorError> {
        self.build_with_backend(
            crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
            false,
            false,
            |program, options| {
                let mut trace = crate::debug::CompilationTrace::default();
                let wants_trace = options.trace.pre_optimized_clif
                    || options.trace.post_optimized_clif
                    || options.trace.native;
                let backend = JitBackend::new(program, options, wants_trace.then_some(&mut trace))?;
                if options.trace.output_to_stdout {
                    trace.print();
                }
                Ok(backend)
            },
        )
    }

    /// Compiles a timed simulation using the host's native backend.
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    pub fn build_native(
        mut self,
    ) -> Result<crate::Simulation<crate::backend::native::NativeBackend>, SimulatorError> {
        self.native_file_calls = true;
        self.build_with_backend(
            crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
            true,
            true,
            |program, options| {
                if !(options.trace.mir || options.trace.native) {
                    return crate::backend::native::NativeBackend::new(program, options);
                }
                let (backend, native_trace) =
                    crate::backend::native::NativeBackend::new_with_codegen_trace(
                        program, options,
                    )?;
                if options.trace.output_to_stdout {
                    crate::debug::CompilationTrace {
                        native_optimized_sir: Some(native_trace.optimized_sir),
                        mir: options.trace.mir.then_some(native_trace.mir),
                        ..crate::debug::CompilationTrace::default()
                    }
                    .print();
                }
                Ok(backend)
            },
        )
    }

    /// Compiles a timed simulation into a native image without loading
    /// it. The image carries the process kernels and the compiled
    /// scheduler; [`NativeCompilation::initialize`] or
    /// [`Self::build_native_from_image`] starts a simulation of it.
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub fn compile_native(
        mut self,
    ) -> Result<NativeCompilation<crate::Simulation>, SimulatorError> {
        self.native_file_calls = true;
        self.options.emit_triggers = true;
        self.options.host_edges = false;
        let (laid_out, mut warnings, options, vcd_path, injected_components) = self
            .into_laid_out_program(
                crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
                true,
                true,
            )?;
        let image = crate::backend::native::NativeBackend::compile_image(&laid_out, &options)
            .map_err(|error| error.with_warnings(std::mem::take(&mut warnings)))?;
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

    /// Starts a timed simulation of a native image from
    /// [`Self::compile_native`], without code generation. The design's
    /// runtime metadata comes from `image`; the sources of this builder
    /// are not used, its runtime options (scheduler, DPI symbols,
    /// plusargs, VCD output) are.
    ///
    /// An untimed RTL image without a native testbench has no compiled
    /// scheduler, so its time slots run in the reference loop.
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub fn build_native_from_image(
        self,
        image: crate::backend::native::NativeProgramImage,
    ) -> Result<crate::Simulation<crate::backend::native::NativeBackend>, SimulatorError> {
        let mut options = self.options;
        options.four_state = image.four_state();
        options.emit_triggers = true;
        let program = image.runtime_program();
        Self::start_native_image(
            image,
            program,
            Vec::new(),
            options,
            self.vcd_path,
            self.injected_components,
        )
    }

    /// Compiles a timed simulation using the Wasmtime WASM backend.
    #[cfg(feature = "wasm-runtime")]
    pub fn build_wasm(
        self,
    ) -> Result<crate::Simulation<crate::backend::wasm_runtime::WasmBackend>, SimulatorError> {
        self.build_with_backend(
            crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
            false,
            false,
            crate::backend::wasm_runtime::WasmBackend::new,
        )
    }

    /// Builds a timed simulation using the interpreter backend.
    pub fn build_interpreter(
        self,
    ) -> Result<crate::Simulation<crate::backend::InterpBackend>, SimulatorError> {
        self.build_with_backend(
            crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
            false,
            false,
            crate::backend::InterpBackend::new,
        )
    }

    /// Builds a timed simulation that starts interpreted and adopts compiled
    /// code at a scheduler safe point once background compilation completes.
    pub fn build_tiered(
        mut self,
    ) -> Result<crate::Simulation<crate::backend::TieredBackend>, SimulatorError> {
        let layout = crate::backend::tiered::tiered_layout_mode(self.vcd_path.is_some());
        self.native_file_calls = true;
        // The interpreted tier shows the updates of its units when they
        // end; the compiled one does too, so that both show the same.
        self.value_change_points = false;
        // The interpreter and hotspot tiers walk derived-tick plans, and
        // only a native image runs derived-edge units; it expands deferred
        // ones itself. Expanding every unit here copies the comb settle
        // into each of them, which exhausts memory on large designs.
        let defer_derived_edges = crate::backend::tiered::native_is_default_target();
        // Wide FF commits match the native build where the compiled
        // tier is a native image, which alone compiles them; the
        // units keep their typed shadow commits for the interpreter.
        // A Cranelift tier gets none.
        let wide_commits = crate::backend::tiered::native_is_default_target();
        self.build_with_shared_backend(
            layout,
            defer_derived_edges,
            wide_commits,
            |program, options| Ok(crate::backend::TieredBackend::new_shared(program, options)),
        )
    }

    #[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
    pub(crate) fn build_scheduler_test(
        mut self,
    ) -> Result<crate::Simulation<crate::backend::TieredBackend>, SimulatorError> {
        self.native_file_calls = true;
        let publish =
            self.options.hotspot_jit && self.options.scheduler != crate::SchedulerMode::Reference;
        let mut simulation = self.build_with_backend(
            crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
            false,
            false,
            crate::backend::TieredBackend::for_scheduler_test,
        )?;
        if publish {
            simulation.simulator.backend.publish_scheduler_for_test();
        }
        Ok(simulation)
    }

    /// Compiles the design up to the scheduler plan, as [`Self::build`]
    /// does, but generates no code, and reports the plan: per process
    /// kernel where it comes from, whether the compiled scheduler runs it
    /// (or why not) and the events it writes, the scheduler's events and
    /// clocks, the sizes of the SIR units, and the warnings a build would
    /// return. For inspecting and timing elaboration without code
    /// generation.
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    pub fn build_plan(mut self) -> Result<String, SimulatorError> {
        self.native_file_calls = true;
        self.options.emit_triggers = true;
        self.options.host_edges = false;
        let (laid_out, warnings, ..) = self.into_laid_out_program(
            crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
            true,
            true,
        )?;
        Ok(plan_report(&laid_out, &warnings))
    }

    pub(super) fn build_with_backend<B: crate::SimBackend>(
        self,
        layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
        defer_derived_edges: bool,
        wide_commits: bool,
        compile: impl Fn(&crate::ir::LaidOutProgram, &SimulatorOptions) -> Result<B, SimulatorError>,
    ) -> Result<crate::Simulation<B>, SimulatorError> {
        self.build_with_shared_backend(
            layout_mode,
            defer_derived_edges,
            wide_commits,
            |program, options| compile(program, options),
        )
    }

    /// [`Self::build_with_backend`] for a backend that may keep the program
    /// it is built from, sharing it instead of copying it.
    pub(super) fn build_with_shared_backend<B: crate::SimBackend>(
        mut self,
        layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
        defer_derived_edges: bool,
        wide_commits: bool,
        compile: impl Fn(
            &std::sync::Arc<crate::ir::LaidOutProgram>,
            &SimulatorOptions,
        ) -> Result<B, SimulatorError>,
    ) -> Result<crate::Simulation<B>, SimulatorError> {
        self.options.emit_triggers = true;
        self.options.host_edges = false;
        let (laid_out, mut warnings, options, vcd_path, injected_components) =
            self.into_laid_out_program(layout_mode, defer_derived_edges, wide_commits)?;
        let laid_out = std::sync::Arc::new(laid_out);
        let backend_start = options.diagnostics.phase_timing.then(crate::timing::now);
        let backend = compile(&laid_out, &options)
            .map_err(|error| error.with_warnings(std::mem::take(&mut warnings)))?;
        if let Some(start) = backend_start {
            tracing::debug!("[phase-timing] backend_codegen: {:?}", start.elapsed());
        }
        let shadow = Self::start_shadow(
            |shadow_options| compile(&laid_out, shadow_options),
            &laid_out.runtime,
            &options,
            &injected_components,
        )?;
        // A backend that kept the program leaves only its runtime metadata
        // to copy.
        let runtime = match std::sync::Arc::try_unwrap(laid_out) {
            Ok(laid_out) => laid_out.into_runtime(),
            Err(shared) => shared.runtime().clone(),
        };
        let mut simulation = Self::start_simulation(
            backend,
            runtime,
            warnings,
            &options,
            vcd_path,
            injected_components,
        )?;
        if let Some(shadow) = shadow {
            simulation.set_shadow(shadow);
        }
        Ok(simulation)
    }

    /// The reference-loop shadow of a differential simulation of
    /// `program`, with `backend` providing its own copy of the code, or
    /// why it has none. Other scheduler modes have no shadow.
    #[allow(clippy::type_complexity)]
    pub(super) fn start_shadow<B: crate::SimBackend>(
        backend: impl FnOnce(&SimulatorOptions) -> Result<B, SimulatorError>,
        program: &crate::ir::RuntimeProgram,
        options: &SimulatorOptions,
        injected_components: &crate::InjectedComponents,
    ) -> Result<Option<Result<Box<crate::Simulation<B>>, &'static str>>, SimulatorError> {
        if !matches!(
            options.scheduler,
            crate::SchedulerMode::Differential | crate::SchedulerMode::DifferentialRuns
        ) {
            return Ok(None);
        }
        if !injected_components.definitions.is_empty() {
            return Ok(Some(Err("the design has external components")));
        }
        let mut shadow_options = options.clone();
        shadow_options.trace = Default::default();
        shadow_options.scheduler = crate::SchedulerMode::Reference;
        let backend = backend(&shadow_options)?;
        let shadow = Self::start_simulation(
            backend,
            program.clone(),
            Vec::new(),
            &shadow_options,
            None,
            Default::default(),
        )?;
        // Host state outside the memory image, such as open files,
        // cannot be shared with a shadow.
        Ok(Some(match shadow.simulator.check_host_restore_support() {
            Ok(()) => Ok(Box::new(shadow)),
            Err(_) => Err("the design keeps host state outside its memory image"),
        }))
    }

    /// Load `image` into executable memory and start a timed simulation
    /// of `program`, its runtime metadata. A differential simulation
    /// loads a second copy of the image for its shadow.
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    pub(super) fn start_native_image(
        image: crate::backend::native::NativeProgramImage,
        program: crate::ir::RuntimeProgram,
        warnings: Vec<CompilationWarning>,
        options: SimulatorOptions,
        vcd_path: Option<(std::path::PathBuf, crate::WaveformFormat)>,
        injected_components: crate::InjectedComponents,
    ) -> Result<crate::Simulation<crate::backend::native::NativeBackend>, SimulatorError> {
        if let Err(error) = validate_native_vcd_layout(image.layout(), &program, &vcd_path) {
            return Err(error.with_warnings(warnings));
        }
        let load = |image, options: &SimulatorOptions| {
            // Safety: callers either produced the image in this process
            // or loaded it from an explicitly trusted native-image
            // artifact, and `from_image` validates its structure first.
            unsafe {
                crate::backend::native::NativeBackend::from_image_with_dpi(image, &options.dpi)
            }
        };
        let shadow = match Self::start_shadow(
            |shadow_options| load(image.clone(), shadow_options),
            &program,
            &options,
            &injected_components,
        ) {
            Ok(shadow) => shadow,
            Err(error) => return Err(error.with_warnings(warnings)),
        };
        let backend = match load(image, &options) {
            Ok(backend) => backend,
            Err(error) => return Err(error.with_warnings(warnings)),
        };
        let mut simulation = Self::start_simulation(
            backend,
            program,
            warnings,
            &options,
            vcd_path,
            injected_components,
        )?;
        if let Some(shadow) = shadow {
            simulation.set_shadow(shadow);
        }
        Ok(simulation)
    }

    /// A cross-compiling build cannot execute the code of a native image.
    #[cfg(any(
        all(feature = "arm64-codegen", not(target_arch = "aarch64")),
        all(feature = "x86_64-codegen", not(target_arch = "x86_64"))
    ))]
    pub(super) fn start_native_image(
        image: crate::backend::native::NativeProgramImage,
        program: crate::ir::RuntimeProgram,
        warnings: Vec<CompilationWarning>,
        options: SimulatorOptions,
        vcd_path: Option<(std::path::PathBuf, crate::WaveformFormat)>,
        injected_components: crate::InjectedComponents,
    ) -> Result<crate::Simulation<crate::backend::native::NativeBackend>, SimulatorError> {
        let _ = (image, program, options, vcd_path, injected_components);
        Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError).with_warnings(warnings))
    }

    /// Initialize a timed simulation of `program` compiled into
    /// `backend`.
    pub(super) fn start_simulation<B: crate::SimBackend>(
        backend: B,
        program: crate::ir::RuntimeProgram,
        warnings: Vec<CompilationWarning>,
        options: &SimulatorOptions,
        vcd_path: Option<(std::path::PathBuf, crate::WaveformFormat)>,
        injected_components: crate::InjectedComponents,
    ) -> Result<crate::Simulation<B>, SimulatorError> {
        // Initial kernels must keep their time-zero gates until the timed
        // scheduler starts them; ordinary Simulator builders release them.
        let mut sim =
            Simulator::with_backend_and_program_for_simulation(backend, program, warnings);
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
        // Runtime static declarations precede every comb activation.
        if !options.defer_initialization
            && !sim
                .program
                .runtime_schema
                .processes
                .iter()
                .any(|slots| slots.kind == celox_design::ProcessKind::Initializer)
        {
            sim.modify(|_| {}).map_err(|error| {
                SimulatorError::from(error).with_warnings(std::mem::take(&mut sim.warnings))
            })?;
        }
        let warnings = std::mem::take(&mut sim.warnings);
        match crate::Simulation::try_new_with_initialization(sim, !options.defer_initialization) {
            Ok(mut simulation) => {
                simulation.simulator.warnings = warnings;
                simulation.state.set_scheduler_mode(options.scheduler);
                Ok(simulation)
            }
            Err(error) => Err(SimulatorError::from(error).with_warnings(warnings)),
        }
    }
}
