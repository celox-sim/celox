use std::path::Path;

use veryl_analyzer::ir::{Comptime, Expression, VarPath};
use veryl_analyzer::value::Value;
use veryl_analyzer::{Analyzer, AnalyzerError, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::{ClockType, Metadata, ResetType};
use veryl_parser::Parser;
use veryl_parser::resource_table;

use crate::parser::BuildConfig;
use crate::{
    CompilationWarning, FrontendDiagnostic, ParserError, SimulatorError, SimulatorErrorKind,
    ir::OptimizedSir, parser,
};

fn analyze(
    sources: &[(&str, &Path)],
    top: &str,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    trace_out: Option<&mut crate::debug::CompilationTrace>,
    metadata: Option<Metadata>,
    clock_type: Option<ClockType>,
    reset_type: Option<ResetType>,
    param_overrides: &[(String, u64)],
    optimize_options: &crate::optimizer::OptimizeOptions,
    diagnostics: &crate::RuntimeDiagnostics,
    preserve_element_storage_layout: bool,
) -> (
    Result<OptimizedSir, ParserError>,
    Vec<AnalyzerError>,
    Vec<FrontendDiagnostic>,
) {
    symbol_table::clear();
    attribute_table::clear();

    let metadata = metadata.unwrap_or_else(|| Metadata::create_default("prj").unwrap());
    // Preserve an explicitly configured seed, but defer generating an
    // implicit seed until testbench execution. This keeps compilation
    // deterministic and avoids host-only time APIs in the browser compiler.
    let testbench_random_seed = metadata.test.seed;
    let analyzer = Analyzer::new(&metadata);
    let project_name = metadata.project.name.clone();

    // Per-file: parse + pass1
    let mut parsers = Vec::new();
    let mut errors = vec![];
    for (code, path) in sources {
        let parsed = Parser::parse(code, path).unwrap();
        errors.append(&mut analyzer.analyze_pass1(&project_name, &parsed.veryl));
        parsers.push(parsed);
    }
    let loop_sources =
        parser::loop_provenance::LoopSourceTable::collect(parsers.iter().map(|x| &x.veryl));

    // Global post-pass1
    errors.append(&mut Analyzer::analyze_post_pass1());

    // Shared context for pass2
    let mut context = Context::default();

    if !param_overrides.is_empty() {
        let mut override_map = fxhash::FxHashMap::default();
        let token = veryl_parser::token_range::TokenRange::default();
        for (name, value) in param_overrides {
            let name_id = resource_table::insert_str(name);
            let path = VarPath::new(name_id);
            let val = Value::new(*value, 64, false);
            let comptime = Comptime::create_value(val.clone(), token);
            let expr = Expression::create_value(val, token);
            override_map.insert(path, (comptime, expr));
        }
        context.push_override(override_map);
    }

    let mut ir = Ir::default();

    for parsed in &parsers {
        errors.append(&mut analyzer.analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir)));
    }
    errors.append(&mut Analyzer::analyze_post_pass2(&ir));

    // Veryl reports combinational loops before Celox can apply its path-level
    // false-loop and true-loop authorizations. When the caller supplied such
    // an authorization, defer loop validation to the Celox scheduler: it will
    // still reject every cycle that is not covered by the supplied paths.
    if !ignored_loops.is_empty() || !true_loops.is_empty() {
        errors.retain(|error| !matches!(error, AnalyzerError::CombinationalLoop { .. }));
    }

    let mut frontend_diagnostics = if errors.iter().any(AnalyzerError::is_error) {
        Vec::new()
    } else {
        celox_frontend_veryl::check_dynamic_for_bounds(&ir)
    };
    let loop_provenance = loop_sources.match_unrolled(&ir);

    let top = veryl_parser::resource_table::insert_str(top);
    let mut build_config = BuildConfig::from(&metadata.build);
    if let Some(ct) = clock_type {
        build_config.clock_type = ct;
    }
    if let Some(rt) = reset_type {
        build_config.reset_type = rt;
    }
    let sir = parser::parse(
        &top,
        &ir,
        &loop_provenance,
        &build_config,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        optimize_options,
        diagnostics,
        preserve_element_storage_layout,
        testbench_random_seed,
    )
    .map(|(sir, mut elaborated_diagnostics)| {
        frontend_diagnostics.append(&mut elaborated_diagnostics);
        sir
    });
    (sir, errors, frontend_diagnostics)
}

/// Compile Veryl source code to the SIR (Simulation IR) representation.
///
/// This is the shared compilation pipeline used by all backends.
/// Returns verified optimized SIR and any compilation warnings on success.
pub fn compile_to_sir(
    sources: &[(&str, &Path)],
    top: &str,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    trace_out: Option<&mut crate::debug::CompilationTrace>,
    metadata: Option<Metadata>,
    clock_type: Option<ClockType>,
    reset_type: Option<ResetType>,
    param_overrides: &[(String, u64)],
    optimize_options: &crate::optimizer::OptimizeOptions,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    compile_to_sir_with_layout_mode(
        sources,
        top,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        metadata,
        clock_type,
        reset_type,
        param_overrides,
        optimize_options,
        &crate::RuntimeDiagnostics::default(),
        crate::backend::memory_layout::MemoryLayoutMode::Packed,
    )
}

fn compile_to_sir_with_layout_mode(
    sources: &[(&str, &Path)],
    top: &str,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    trace_out: Option<&mut crate::debug::CompilationTrace>,
    metadata: Option<Metadata>,
    clock_type: Option<ClockType>,
    reset_type: Option<ResetType>,
    param_overrides: &[(String, u64)],
    optimize_options: &crate::optimizer::OptimizeOptions,
    diagnostics: &crate::RuntimeDiagnostics,
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    let (sir, errors, frontend_diagnostics) = analyze(
        sources,
        top,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        metadata,
        clock_type,
        reset_type,
        param_overrides,
        optimize_options,
        diagnostics,
        layout_mode == crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
    );
    let (real_errors, analyzer_warnings): (Vec<_>, Vec<_>) =
        errors.into_iter().partition(AnalyzerError::is_error);
    let (frontend_errors, frontend_warnings): (Vec<_>, Vec<_>) = frontend_diagnostics
        .into_iter()
        .partition(FrontendDiagnostic::is_error);
    let warnings = analyzer_warnings
        .into_iter()
        .map(CompilationWarning::Analyzer)
        .chain(
            frontend_warnings
                .into_iter()
                .map(CompilationWarning::Frontend),
        )
        .collect::<Vec<_>>();
    if !real_errors.is_empty() {
        return Err(
            SimulatorError::new(SimulatorErrorKind::Analyzer(real_errors)).with_warnings(warnings),
        );
    }
    if !frontend_errors.is_empty() {
        return Err(
            SimulatorError::new(SimulatorErrorKind::Frontend(frontend_errors))
                .with_warnings(warnings),
        );
    }
    match sir {
        Ok(p) => Ok((p, warnings)),
        Err(e) => Err(SimulatorError::from(e).with_warnings(warnings)),
    }
}

// ── JIT-specific types and builders (native only) ────────────────────

#[cfg(feature = "host-runtime")]
mod host {
    use super::super::Simulator;
    use super::*;
    use crate::backend::JitBackend;
    use crate::ir::LaidOutProgram;

    /// Controls which stores the dead store elimination pass preserves.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum DeadStorePolicy {
        /// Keep all stores (no dead store elimination). Default for user-facing builds.
        #[default]
        Off,
        /// Eliminate stores except those explicitly marked live via `live_signal()`
        /// and those loaded by execution units.
        PreserveListedSignals,
        /// Eliminate stores except those to top-module ports and those loaded by EUs.
        PreserveTopPorts,
        /// Eliminate stores except those to ports of *all* instances and those loaded by EUs.
        PreserveAllPorts,
    }

    #[derive(Debug, Clone)]
    pub struct SimulatorOptions {
        pub four_state: bool,
        /// Per-pass SIRT optimizer flags.
        pub optimize_options: crate::optimizer::OptimizeOptions,
        /// Fine-grained Cranelift backend options.
        pub cranelift_options: crate::backend::CraneliftOptions,
        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub x86_options: crate::backend::X86BackendOptions,
        pub trace: crate::debug::TraceOptions,
        pub diagnostics: crate::RuntimeDiagnostics,
        /// When true, JIT-compiled functions emit trigger detection code for
        /// edge-based event discovery. Only needed by [`crate::Simulation`].
        pub emit_triggers: bool,
        /// Dead store elimination policy.
        pub dead_store_policy: DeadStorePolicy,
    }

    impl Default for SimulatorOptions {
        fn default() -> Self {
            let opt = crate::optimizer::OptimizeOptions::default();
            Self {
                four_state: false,
                optimize_options: opt,
                cranelift_options: crate::backend::CraneliftOptions::default(),
                #[cfg(any(
                    target_arch = "x86_64",
                    all(target_arch = "aarch64", feature = "experimental-arm64-backend")
                ))]
                x86_options: crate::backend::X86BackendOptions::default(),
                trace: Default::default(),
                diagnostics: Default::default(),
                emit_triggers: false,
                dead_store_policy: DeadStorePolicy::Off,
            }
        }
    }

    /// A fluent builder for configuring and initializing a [`Simulator`] or
    /// [`Simulation`](crate::Simulation).
    ///
    /// Use [`Simulator::builder()`] or [`Simulation::builder()`](crate::Simulation::builder)
    /// to obtain the appropriate variant. Both share the same configuration methods;
    /// only `.build()` differs in return type.
    pub struct SimulatorBuilder<'a, Target = Simulator> {
        sources: Vec<(&'a str, &'a Path)>,
        top: &'a str,
        ignored_loops: Vec<(
            (Vec<(String, usize)>, Vec<String>),
            (Vec<(String, usize)>, Vec<String>),
        )>,
        true_loops: Vec<(
            (Vec<(String, usize)>, Vec<String>),
            (Vec<(String, usize)>, Vec<String>),
            usize,
        )>,
        options: SimulatorOptions,
        vcd_path: Option<std::path::PathBuf>,
        metadata: Option<Metadata>,
        clock_type: Option<ClockType>,
        reset_type: Option<ResetType>,
        param_overrides: Vec<(String, u64)>,
        live_signals: Vec<(Vec<(String, usize)>, Vec<String>)>,
        _marker: std::marker::PhantomData<Target>,
    }

    /// Configuration methods shared by all builder variants.
    impl<'a, Target> SimulatorBuilder<'a, Target> {
        /// Returns the source files passed to this builder.
        pub fn sources(&self) -> &[(&'a str, &'a Path)] {
            &self.sources
        }

        /// Returns the top module name.
        pub fn top(&self) -> &'a str {
            self.top
        }

        /// Supply project metadata (clock/reset settings, etc.) instead of defaults.
        pub fn with_metadata(mut self, metadata: Metadata) -> Self {
            self.metadata = Some(metadata);
            self
        }

        /// Override the clock type (posedge/negedge) from metadata or defaults.
        pub fn clock_type(mut self, clock_type: ClockType) -> Self {
            self.clock_type = Some(clock_type);
            self
        }

        /// Override the reset type (async_high/async_low/sync_high/sync_low) from metadata or defaults.
        pub fn reset_type(mut self, reset_type: ResetType) -> Self {
            self.reset_type = Some(reset_type);
            self
        }

        /// Override a top-level module parameter value.
        pub fn param(mut self, name: &str, value: u64) -> Self {
            self.param_overrides.push((name.to_string(), value));
            self
        }

        /// Enable VCD dumping to the specified file.
        pub fn vcd<P: AsRef<std::path::Path>>(mut self, path: P) -> Self {
            self.vcd_path = Some(path.as_ref().to_path_buf());
            self
        }

        /// Enable 4-state (0, 1, X, Z) simulation mode.
        pub fn four_state(mut self, enable: bool) -> Self {
            self.options.four_state = enable;
            self
        }

        /// Set the overall optimization level. Sets defaults for SIR passes,
        /// Cranelift options, and DSE policy. Per-pass overrides can be applied after.
        pub fn opt_level(mut self, level: crate::optimizer::OptLevel) -> Self {
            self.options.optimize_options = crate::optimizer::OptimizeOptions::new(level);
            self.options.cranelift_options =
                crate::backend::CraneliftOptions::for_speed_optimization(
                    level != crate::optimizer::OptLevel::O0,
                );
            self.options.dead_store_policy = match level {
                crate::optimizer::OptLevel::O2 => DeadStorePolicy::PreserveTopPorts,
                _ => DeadStorePolicy::Off,
            };
            self
        }

        /// Enable a specific SIR pass, overriding the OptLevel default.
        pub fn enable_pass(mut self, pass: crate::optimizer::SirPass) -> Self {
            if pass == crate::optimizer::SirPass::TailCallSplit {
                self.options.cranelift_options.tail_call_split = true;
            }
            self.options.optimize_options = self.options.optimize_options.enable(pass);
            self
        }

        /// Disable a specific SIR pass, overriding the OptLevel default.
        pub fn disable_pass(mut self, pass: crate::optimizer::SirPass) -> Self {
            if pass == crate::optimizer::SirPass::TailCallSplit {
                self.options.cranelift_options.tail_call_split = false;
            }
            self.options.optimize_options = self.options.optimize_options.disable(pass);
            self
        }

        /// Enable or disable all SIRT optimization passes at once.
        /// Shorthand: `true` → `OptLevel::O1`, `false` → `OptLevel::O0`.
        pub fn optimize(mut self, enable: bool) -> Self {
            self.options.optimize_options = if enable {
                crate::optimizer::OptimizeOptions::all()
            } else {
                crate::optimizer::OptimizeOptions::none()
            };
            self
        }

        /// Set per-pass optimizer flags directly.
        pub fn optimize_options(mut self, options: crate::optimizer::OptimizeOptions) -> Self {
            self.options.cranelift_options.tail_call_split =
                options.is_enabled(crate::optimizer::SirPass::TailCallSplit);
            self.options.optimize_options = options;
            self
        }

        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub fn x86_slp(mut self, enable: bool) -> Self {
            self.options.x86_options.slp = enable;
            self
        }

        #[cfg(not(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        )))]
        pub fn x86_slp(self, enable: bool) -> Self {
            let _ = enable;
            self
        }

        /// Set fine-grained Cranelift backend options.
        pub fn cranelift_options(mut self, options: crate::backend::CraneliftOptions) -> Self {
            self.options.cranelift_options = options;
            self
        }

        /// Set the register allocator algorithm.
        pub fn regalloc_algorithm(mut self, algo: crate::backend::RegallocAlgorithm) -> Self {
            self.options.cranelift_options.regalloc_algorithm = algo;
            self
        }

        /// Enable or disable alias analysis in the Cranelift egraph pass.
        pub fn enable_alias_analysis(mut self, enable: bool) -> Self {
            self.options.cranelift_options.enable_alias_analysis = enable;
            self
        }

        /// Enable or disable the Cranelift IR verifier.
        pub fn enable_verifier(mut self, enable: bool) -> Self {
            self.options.cranelift_options.enable_verifier = enable;
            self
        }

        /// Set the dead store elimination policy.
        pub fn dead_store_policy(mut self, policy: DeadStorePolicy) -> Self {
            self.options.dead_store_policy = policy;
            self
        }

        /// Mark a signal as externally observable (live) for dead store elimination.
        pub fn live_signal(
            mut self,
            instance_path: Vec<(String, usize)>,
            var_path: Vec<String>,
        ) -> Self {
            self.live_signals.push((instance_path, var_path));
            self
        }

        /// Configure compilation tracing options.
        pub fn trace(mut self, trace: crate::debug::TraceOptions) -> Self {
            self.options.trace = trace;
            self
        }

        /// Configure diagnostics explicitly for this build.
        pub fn diagnostics(mut self, diagnostics: crate::DiagnosticsOptions) -> Self {
            self.options.diagnostics = diagnostics.runtime;
            self.options.optimize_options.diagnostics = diagnostics.sir;
            self.options.cranelift_options.diagnostics = diagnostics.cranelift;
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            {
                self.options.x86_options.diagnostics = diagnostics.native;
                if let Some(enabled) = diagnostics.native_tick_loop {
                    self.options.x86_options.native_tick_loop = enabled;
                }
            }
            self
        }

        /// Import legacy `CELOX_*` diagnostics switches once at the API boundary.
        pub fn diagnostics_from_env(self) -> Self {
            self.diagnostics(crate::DiagnosticsOptions::from_env())
        }

        pub fn trace_sim_modules(mut self) -> Self {
            self.options.trace.sim_modules = true;
            self
        }

        pub fn trace_pre_atomized_comb_blocks(mut self) -> Self {
            self.options.trace.pre_atomized_comb_blocks = true;
            self
        }

        pub fn trace_atomized_comb_blocks(mut self) -> Self {
            self.options.trace.atomized_comb_blocks = true;
            self
        }

        pub fn trace_flattened_comb_blocks(mut self) -> Self {
            self.options.trace.flattened_comb_blocks = true;
            self
        }

        pub fn trace_scheduled_units(mut self) -> Self {
            self.options.trace.scheduled_units = true;
            self
        }

        pub fn trace_pre_optimized_sir(mut self) -> Self {
            self.options.trace.pre_optimized_sir = true;
            self
        }

        pub fn trace_post_optimized_sir(mut self) -> Self {
            self.options.trace.post_optimized_sir = true;
            self
        }

        pub fn trace_analyzer_ir(mut self) -> Self {
            self.options.trace.analyzer_ir = true;
            self
        }

        pub fn trace_pre_optimized_clif(mut self) -> Self {
            self.options.trace.pre_optimized_clif = true;
            self
        }

        pub fn trace_post_optimized_clif(mut self) -> Self {
            self.options.trace.post_optimized_clif = true;
            self
        }

        pub fn trace_native(mut self) -> Self {
            self.options.trace.native = true;
            self
        }

        pub fn trace_mir(mut self) -> Self {
            self.options.trace.mir = true;
            self
        }

        /// Add one profile-selected native JIT block to state-layout feasibility
        /// analysis. The analysis is captured by [`Self::build_with_trace`] from
        /// the exact merged SIR passed to native instruction selection.
        pub fn trace_native_profile_block(
            mut self,
            function: impl Into<String>,
            block: u32,
            samples: u64,
        ) -> Self {
            self.options
                .trace
                .native_profile_blocks
                .push(crate::debug::NativeProfileBlock {
                    function: function.into(),
                    block,
                    samples,
                });
            self
        }

        pub fn trace_on_build(mut self) -> Self {
            self.options.trace.output_to_stdout = true;
            self
        }

        /// Explicitly ignore a dependency between two signals.
        pub fn false_loop(
            mut self,
            from: (Vec<(String, usize)>, Vec<String>),
            to: (Vec<(String, usize)>, Vec<String>),
        ) -> Self {
            self.ignored_loops.push((from, to));
            self
        }

        /// Mark a dependency as a "true loop" and specify its convergence limit.
        pub fn true_loop(
            mut self,
            from: (Vec<(String, usize)>, Vec<String>),
            to: (Vec<(String, usize)>, Vec<String>),
            max_iter: usize,
        ) -> Self {
            self.true_loops.push((from, to, max_iter));
            self
        }
    }

    impl<'a> SimulatorBuilder<'a, Simulator> {
        pub fn new(code: &'a str, top: &'a str) -> Self {
            Self {
                sources: vec![(code, Path::new(""))],
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                _marker: std::marker::PhantomData,
            }
        }

        pub fn from_sources(sources: Vec<(&'a str, &'a Path)>, top: &'a str) -> Self {
            Self {
                sources,
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                _marker: std::marker::PhantomData,
            }
        }

        /// Compile SIR, finalize its state layout, and return the typed artifact
        /// along with the remaining builder state.
        /// Consumes self.
        fn into_laid_out_program(
            self,
            layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
        ) -> Result<
            (
                crate::ir::LaidOutProgram,
                Vec<CompilationWarning>,
                SimulatorOptions,
                Option<std::path::PathBuf>,
            ),
            SimulatorError,
        > {
            let phase_timing = self.options.diagnostics.phase_timing;
            let compile_start = phase_timing.then(crate::timing::now);
            let (program, warnings) = compile_to_sir_with_layout_mode(
                &self.sources,
                self.top,
                &self.ignored_loops,
                &self.true_loops,
                self.options.four_state,
                &self.options.trace,
                None,
                self.metadata,
                self.clock_type,
                self.reset_type,
                &self.param_overrides,
                &self.options.optimize_options,
                &self.options.diagnostics,
                layout_mode,
            )?;
            if let Some(start) = compile_start {
                tracing::debug!("[phase-timing] compile_to_sir: {:?}", start.elapsed());
            }

            // Build memory layout (consumes semantic layout requirements).
            let layout_start = phase_timing.then(crate::timing::now);
            let mut laid_out =
                program.into_laid_out_with_mode(self.options.four_state, layout_mode);
            if let Some(start) = layout_start {
                tracing::debug!("[phase-timing] build_layout: {:?}", start.elapsed());
            }

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

            Ok((laid_out, warnings, self.options, self.vcd_path))
        }

        /// Compiles the Veryl source and constructs the simulator.
        /// Uses a custom native backend on x86-64 and opt-in AArch64, Cranelift elsewhere.
        pub fn build(self) -> Result<Simulator<crate::DefaultBackend>, SimulatorError> {
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            {
                self.build_native()
            }
            #[cfg(not(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            )))]
            {
                self.build_cranelift()
            }
        }

        /// Compiles using the Cranelift JIT backend.
        pub fn build_cranelift(self) -> Result<Simulator<JitBackend>, SimulatorError> {
            let phase_timing = self.options.diagnostics.phase_timing;
            let phase_start = phase_timing.then(crate::timing::now);

            let (laid_out, warnings, options, vcd_path) = self
                .into_laid_out_program(crate::backend::memory_layout::MemoryLayoutMode::Packed)?;

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
            )?;
            if options.trace.output_to_stdout {
                trace.print();
            }
            if let Some(s) = jit_start {
                tracing::debug!("[phase-timing] jit_backend: {:?}", s.elapsed());
            }

            let mut sim =
                Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
            sim.diagnostics = options.diagnostics.clone();
            if let Some(path) = vcd_path {
                let descs = sim.build_vcd_descs(options.four_state);
                let vcd_writer = crate::VcdWriter::new(path, &descs)
                    .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
                sim.vcd_writer = Some(vcd_writer);
            }
            sim.apply_initial_values();
            sim.modify(|_| {}).map_err(SimulatorError::from)?;
            Ok(sim)
        }

        /// Compiles using the custom native backend for this host architecture.
        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub fn build_native(
            self,
        ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
            let phase_timing = self.options.diagnostics.phase_timing;
            let sir_start = phase_timing.then(crate::timing::now);
            let (laid_out, warnings, options, vcd_path) = self.into_laid_out_program(
                crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
            )?;
            if let Some(start) = sir_start {
                tracing::debug!(
                    "[phase-timing] into_laid_out_program total: {:?}",
                    start.elapsed()
                );
            }
            let backend_start = phase_timing.then(crate::timing::now);
            let backend = crate::backend::native::NativeBackend::new(&laid_out, &options)?;
            if let Some(start) = backend_start {
                tracing::debug!("[phase-timing] native_backend: {:?}", start.elapsed());
            }
            let mut sim =
                Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
            sim.diagnostics = options.diagnostics.clone();
            if let Some(path) = vcd_path {
                let descs = sim.build_vcd_descs(options.four_state);
                let vcd_writer = crate::VcdWriter::new(path, &descs)
                    .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
                sim.vcd_writer = Some(vcd_writer);
            }
            let apply_initial_start = phase_timing.then(crate::timing::now);
            sim.apply_initial_values();
            if let Some(start) = apply_initial_start {
                tracing::debug!("[phase-timing] apply_initial_values: {:?}", start.elapsed());
            }
            let settle_start = phase_timing.then(crate::timing::now);
            sim.modify(|_| {}).map_err(SimulatorError::from)?;
            if let Some(start) = settle_start {
                tracing::debug!("[phase-timing] initial_settle: {:?}", start.elapsed());
            }
            Ok(sim)
        }

        /// Compiles using the Wasmtime WASM backend.
        pub fn build_wasm(
            self,
        ) -> Result<Simulator<crate::backend::wasm_runtime::WasmBackend>, SimulatorError> {
            let (laid_out, warnings, options, vcd_path) = self
                .into_laid_out_program(crate::backend::memory_layout::MemoryLayoutMode::Packed)?;
            let backend = crate::backend::wasm_runtime::WasmBackend::new(&laid_out, &options)?;
            let mut sim =
                Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
            sim.diagnostics = options.diagnostics.clone();
            if let Some(path) = vcd_path {
                let descs = sim.build_vcd_descs(options.four_state);
                let vcd_writer = crate::VcdWriter::new(path, &descs)
                    .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
                sim.vcd_writer = Some(vcd_writer);
            }
            sim.apply_initial_values();
            sim.modify(|_| {}).map_err(SimulatorError::from)?;
            Ok(sim)
        }

        /// Compiles and runs a native testbench (`#[test]` module).
        pub fn run_test(self) -> Result<crate::testbench::TestResult, SimulatorError> {
            run_test_with_sim(self.build()?)
        }

        /// Compiles and runs a testbench using the Cranelift JIT backend.
        pub fn run_test_cranelift(self) -> Result<crate::testbench::TestResult, SimulatorError> {
            run_test_with_sim(self.build_cranelift()?)
        }

        /// Compiles and runs a testbench using the custom native backend.
        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub fn run_test_native(self) -> Result<crate::testbench::TestResult, SimulatorError> {
            run_test_with_sim(self.build_native()?)
        }

        /// Compiles and runs a native testbench, returning assertion results
        /// observed before the test finishes or stops on a fatal failure.
        pub fn run_test_detailed(
            self,
        ) -> Result<crate::testbench::TestResultDetailed, SimulatorError> {
            let mut sim = self.build()?;
            let testbench = crate::testbench::compile_initial_testbench(&sim).ok_or_else(|| {
                SimulatorError::new(SimulatorErrorKind::Codegen(crate::CodegenError::message(
                    "no initial block found — this module is not a native testbench",
                )))
            })?;
            Ok(crate::testbench::run_testbench_detailed(
                &mut sim, &testbench,
            ))
        }

        /// Compiles the Veryl source and constructs the core logic simulator,
        /// while capturing compilation trace data as configured by TraceOptions.
        pub fn build_with_trace(self) -> crate::debug::CompilationTraceResult {
            let mut trace = crate::debug::CompilationTrace::default();
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            let layout_mode = crate::backend::memory_layout::MemoryLayoutMode::ElementStrided;
            #[cfg(not(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            )))]
            let layout_mode = crate::backend::memory_layout::MemoryLayoutMode::Packed;
            let program_res = compile_to_sir_with_layout_mode(
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
                &self.param_overrides,
                &self.options.optimize_options,
                &self.options.diagnostics,
                layout_mode,
            );

            let sim_res = program_res.and_then(|(program, warnings)| {
                let mut laid_out =
                    program.into_laid_out_with_mode(self.options.four_state, layout_mode);

                if self.options.dead_store_policy != DeadStorePolicy::Off {
                    run_dead_store_elimination(&mut laid_out, &self.live_signals, &self.options);
                }

                #[cfg(any(
                    target_arch = "x86_64",
                    all(target_arch = "aarch64", feature = "experimental-arm64-backend")
                ))]
                let backend = if self.options.trace.mir
                    || !self.options.trace.native_profile_blocks.is_empty()
                {
                    let (backend, native_trace) =
                        crate::backend::native::NativeBackend::new_with_codegen_trace(
                            &laid_out,
                            &self.options,
                        )?;
                    trace.native_optimized_sir = Some(native_trace.optimized_sir);
                    trace.mir = Some(native_trace.mir);
                    trace.reactive_event_graph = Some(native_trace.reactive_graph);
                    trace.native_state_layout = Some(native_trace.state_layout);
                    backend
                } else {
                    crate::backend::native::NativeBackend::new(&laid_out, &self.options)?
                };
                #[cfg(not(any(
                    target_arch = "x86_64",
                    all(target_arch = "aarch64", feature = "experimental-arm64-backend")
                )))]
                let backend = JitBackend::new(&laid_out, &self.options, None)?;

                let mut sim =
                    Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
                sim.diagnostics = self.options.diagnostics.clone();
                sim.apply_initial_values();
                sim.modify(|_| {}).map_err(SimulatorError::from)?;
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

    fn run_test_with_sim<B: crate::backend::SimBackend>(
        mut sim: Simulator<B>,
    ) -> Result<crate::testbench::TestResult, SimulatorError> {
        let phase_timing = sim.diagnostics.phase_timing;
        let testbench_start = phase_timing.then(crate::timing::now);
        let testbench = crate::testbench::compile_initial_testbench(&sim).ok_or_else(|| {
            SimulatorError::new(SimulatorErrorKind::Codegen(crate::CodegenError::message(
                "no initial block found — this module is not a native testbench",
            )))
        })?;
        let result = crate::testbench::run_testbench(&mut sim, &testbench);
        if let Some(start) = testbench_start {
            tracing::debug!("[phase-timing] testbench: {:?}", start.elapsed());
        }
        Ok(result)
    }

    impl<'a> SimulatorBuilder<'a, crate::Simulation> {
        pub(crate) fn new(code: &'a str, top: &'a str) -> Self {
            Self {
                sources: vec![(code, Path::new(""))],
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                _marker: std::marker::PhantomData,
            }
        }

        pub(crate) fn from_sources(sources: Vec<(&'a str, &'a Path)>, top: &'a str) -> Self {
            Self {
                sources,
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                _marker: std::marker::PhantomData,
            }
        }

        /// Compiles the Veryl source and constructs the timed simulation wrapper.
        pub fn build(mut self) -> Result<crate::Simulation, SimulatorError> {
            self.options.emit_triggers = true;
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            let layout_mode = crate::backend::memory_layout::MemoryLayoutMode::ElementStrided;
            #[cfg(not(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            )))]
            let layout_mode = crate::backend::memory_layout::MemoryLayoutMode::Packed;
            let (program, warnings) = compile_to_sir_with_layout_mode(
                &self.sources,
                self.top,
                &self.ignored_loops,
                &self.true_loops,
                self.options.four_state,
                &self.options.trace,
                None,
                self.metadata,
                self.clock_type,
                self.reset_type,
                &self.param_overrides,
                &self.options.optimize_options,
                &self.options.diagnostics,
                layout_mode,
            )?;
            let mut laid_out =
                program.into_laid_out_with_mode(self.options.four_state, layout_mode);

            if self.options.dead_store_policy != DeadStorePolicy::Off {
                run_dead_store_elimination(&mut laid_out, &self.live_signals, &self.options);
            }
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            let backend = crate::backend::native::NativeBackend::new(&laid_out, &self.options)?;
            #[cfg(not(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            )))]
            let backend = crate::backend::JitBackend::new(&laid_out, &self.options, None)?;

            let mut sim =
                Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
            sim.diagnostics = self.options.diagnostics.clone();
            if let Some(path) = self.vcd_path {
                let descs = sim.build_vcd_descs(self.options.four_state);
                let vcd_writer = crate::VcdWriter::new(path, &descs)
                    .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
                sim.vcd_writer = Some(vcd_writer);
            }
            sim.apply_initial_values();
            sim.modify(|_| {}).map_err(SimulatorError::from)?;
            Ok(crate::Simulation::new(sim))
        }
    }

    /// Resolve user-specified `(instance_path, var_path)` to `AbsoluteAddr` and run DSE.
    fn run_dead_store_elimination(
        program: &mut LaidOutProgram,
        live_signals: &[(Vec<(String, usize)>, Vec<String>)],
        options: &SimulatorOptions,
    ) {
        use crate::HashSet;
        use crate::ir::InstancePath;
        let mut externally_live = HashSet::default();

        // Native testbench expressions bypass SIR and read simulator memory
        // directly. Their inputs are therefore external DSE roots just like
        // signals named with `live_signal()`.
        externally_live.extend(program.runtime_schema.testbench_read_roots.iter().copied());

        // User-specified live signals
        for (inst_path, var_path) in live_signals {
            let inst_refs: Vec<(&str, usize)> =
                inst_path.iter().map(|(s, i)| (s.as_str(), *i)).collect();
            let var_refs: Vec<&str> = var_path.iter().map(|s| s.as_str()).collect();
            let addr = program.get_addr(&inst_refs, &var_refs).unwrap();
            externally_live.insert(addr);
        }

        // PreserveTopPorts: auto-collect top module port addresses
        if options.dead_store_policy == DeadStorePolicy::PreserveTopPorts {
            if let Some(&top_instance_id) = program.frontend.instance_ids.get(&InstancePath(vec![]))
            {
                if let Some(&top_module_id) = program.frontend.instance_module.get(&top_instance_id)
                {
                    if let Some(top_vars) = program.frontend.module_variables.get(&top_module_id) {
                        for info in top_vars.values() {
                            if info.var_kind.is_port() {
                                if let Some(address) =
                                    program.state_address_for_source(top_instance_id, info.id)
                                {
                                    externally_live.insert(address);
                                }
                            }
                        }
                    }
                }
            }
        }

        // PreserveAllPorts: collect port addresses from every instance
        if options.dead_store_policy == DeadStorePolicy::PreserveAllPorts {
            for (&instance_id, &module_id) in &program.frontend.instance_module {
                if let Some(vars) = program.frontend.module_variables.get(&module_id) {
                    for info in vars.values() {
                        if info.var_kind.is_port() {
                            if let Some(address) =
                                program.state_address_for_source(instance_id, info.id)
                            {
                                externally_live.insert(address);
                            }
                        }
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
}

#[cfg(feature = "host-runtime")]
pub use host::*;
