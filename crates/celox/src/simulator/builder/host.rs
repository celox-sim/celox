use super::super::Simulator;
use super::*;
#[cfg(feature = "cranelift")]
use crate::backend::JitBackend;
use crate::ir::LaidOutProgram;

pub use celox_design::DeadStorePolicy;

mod build;
mod native_compilation;
mod options;
mod simulation;
#[cfg(test)]
mod tests;
pub use native_compilation::*;
pub use options::*;

/// A fluent builder for configuring and initializing a [`Simulator`] or
/// [`Simulation`](crate::Simulation).
///
/// Use [`Simulator::builder()`] or [`Simulation::builder()`](crate::Simulation::builder)
/// to obtain the appropriate variant. Both share the same configuration methods;
/// their build methods return the corresponding simulator or timed simulation.
pub struct SimulatorBuilder<'a, Target = Simulator> {
    sources: Vec<(&'a str, &'a Path)>,
    sv_sources: Vec<(&'a str, &'a Path)>,
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
    vcd_path: Option<(std::path::PathBuf, crate::WaveformFormat)>,
    metadata: Option<Metadata>,
    clock_type: Option<ClockType>,
    reset_type: Option<ResetType>,
    param_overrides: Vec<(String, u64)>,
    sv_param_literals: Vec<(String, String)>,
    live_signals: Vec<(Vec<(String, usize)>, Vec<String>)>,
    injected_components: crate::InjectedComponents,
    frontend_artifact: Option<celox_frontend_sdk::FrontendArtifact>,
    native_file_calls: bool,
    /// Whether the build keeps the value change points of
    /// [`Self::value_change_signals`]: not one that starts interpreted,
    /// whose units have no host context until compiled.
    value_change_points: bool,
    _marker: std::marker::PhantomData<Target>,
}

/// Configuration methods shared by all builder variants.
impl<'a, Target> SimulatorBuilder<'a, Target> {
    /// Returns the source files passed to this builder.
    pub fn sources(&self) -> &[(&'a str, &'a Path)] {
        &self.sources
    }

    /// Returns the SystemVerilog source files passed to this builder.
    pub fn sv_sources(&self) -> &[(&'a str, &'a Path)] {
        &self.sv_sources
    }

    /// Replace the builder's SystemVerilog source set.
    pub fn with_sv_sources(mut self, sources: Vec<(&'a str, &'a Path)>) -> Self {
        self.sv_sources = sources;
        self
    }

    /// Replace the builder's Veryl sources with SystemVerilog sources,
    /// keeping its configuration.
    pub fn into_sv_sources(mut self, sources: Vec<(&'a str, &'a Path)>) -> Self {
        self.sources.clear();
        self.sv_sources = sources;
        self
    }

    /// Returns the top module name.
    pub fn top(&self) -> &str {
        if !self.sources.is_empty() {
            self.top
        } else {
            self.frontend_artifact
                .as_ref()
                .map_or(self.top, celox_frontend_sdk::FrontendArtifact::module_name)
        }
    }

    /// Returns whether four-state simulation is enabled for this builder.
    pub fn four_state_enabled(&self) -> bool {
        self.options.four_state
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
        self.sv_param_literals.retain(|(key, _)| key != name);
        self.param_overrides.push((name.to_string(), value));
        self
    }

    /// Override a pure SV top parameter with a literal, such as `-8`,
    /// `8'hff`, `'1`, or `"XILINX"`. The literal's width and signedness
    /// are preserved. Invalid literals are reported when building.
    pub fn param_literal(mut self, name: &str, literal: &str) -> Self {
        self.param_overrides.retain(|(key, _)| key != name);
        self.sv_param_literals.retain(|(key, _)| key != name);
        self.sv_param_literals
            .push((name.to_string(), literal.to_string()));
        self
    }

    /// Make in-process component implementations available as `$comp::<name>`.
    pub fn with_injected_components(mut self, components: crate::InjectedComponents) -> Self {
        self.injected_components = components;
        self
    }

    /// Enable VCD dumping to the specified file.
    pub fn vcd<P: AsRef<std::path::Path>>(mut self, path: P) -> Self {
        self.vcd_path = Some((path.as_ref().to_path_buf(), crate::WaveformFormat::Vcd));
        self
    }

    /// Enable FST dumping to the specified file. FST is the compressed
    /// binary waveform format read natively by GTKWave and Surfer; it
    /// records the same signals and timestamps as [`Self::vcd`].
    pub fn fst<P: AsRef<std::path::Path>>(mut self, path: P) -> Self {
        self.vcd_path = Some((path.as_ref().to_path_buf(), crate::WaveformFormat::Fst));
        self
    }

    /// Enable 4-state (0, 1, X, Z) simulation mode.
    /// Generate the trigger stores a timed [`crate::Simulation`] relies on.
    #[cfg(test)]
    pub(crate) fn emit_triggers(mut self) -> Self {
        self.options.emit_triggers = true;
        self
    }

    /// Append the generated derived-edge units of every event and record
    /// them in `sink`.
    #[cfg(all(test, feature = "cranelift"))]
    pub(crate) fn derived_edge_units(mut self, sink: crate::derived_edge::TestEdgeUnits) -> Self {
        self.options.derived_edge_units = Some(sink);
        self
    }

    /// Rewrite the optimized SIR with `transform` before layout.
    #[cfg(test)]
    pub(crate) fn transform_sir(mut self, transform: fn(&mut crate::ir::SirProgram)) -> Self {
        self.options.sir_transform = Some(transform);
        self
    }

    /// Supply the host filesystem for runtime file tasks. The factory is
    /// invoked before declaration initialization for each simulator.
    /// Snapshot behavior is defined by the returned [`crate::FileSystem`].
    pub fn file_system(
        mut self,
        factory: impl Fn() -> Box<dyn celox_runtime::file_io_backend::FileSystem>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.options.file_system = celox_runtime::file_io_backend::FileSystemFactory::new(factory);
        self
    }

    /// Set the ordered arguments queried by `$test$plusargs` and
    /// `$value$plusargs`. A leading `+` is optional on each argument.
    /// Queries currently require a timed procedural process, such as an
    /// `initial` block executed by [`crate::Simulation`]. Value queries
    /// support integral conversions and `%s` into packed integral targets;
    /// real conversions and dynamic strings are unsupported.
    pub fn plusargs(mut self, arguments: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.options.plusargs = arguments.into_iter().map(Into::into).collect();
        self
    }

    pub fn four_state(mut self, enable: bool) -> Self {
        self.options.four_state = enable;
        self
    }

    /// Allow incomplete assignments and retained values in `always_comb`.
    ///
    /// Disabled by default. When enabled, inferred latches retain their
    /// value while closed and follow their inputs while open. Explicit
    /// SystemVerilog `always_latch` and ordinary `always @*` processes do
    /// not require this option.
    pub fn allow_inferred_latches(mut self, enable: bool) -> Self {
        self.options.allow_inferred_latches = enable;
        self
    }

    /// Allow function output and non-local write effects in `always_ff`.
    ///
    /// This opts into Celox-specific lowering for constructs rejected by
    /// Veryl's SystemVerilog compatibility checks. It is primarily useful
    /// for testing Celox's FF lowering semantics.
    pub fn allow_always_ff_function_effects(mut self, enable: bool) -> Self {
        self.options.allow_always_ff_function_effects = enable;
        self
    }

    /// Enable native-image support for foreign force/release operations.
    ///
    /// This emits extra combinational entry points, so ordinary simulator
    /// builds leave it disabled. Enabling it forces SIR O0 at build time so
    /// every store boundary needed to reapply a force remains intact.
    pub fn native_force_support(mut self, enable: bool) -> Self {
        self.options.native_force_support = enable;
        self.enforce_native_force_optimizer();
        self.declare_unobserved_nets();
        self
    }

    /// Generate the compiled scheduler so that a slot observer that
    /// watches every update ([`crate::SlotObserver::observes_updates`]),
    /// such as VPI value change callbacks, sees each of them inside
    /// compiled slots. Without it, such an observer's slots run in the
    /// reference loop. The points cost a test of the observer flag each
    /// and keep combinational settles from being deferred.
    pub fn observable_updates(mut self, enable: bool) -> Self {
        self.options.observable_updates = enable;
        self
    }

    /// Show a slot observer that watches every update each blocking
    /// update a process makes of the signals at these hierarchical paths,
    /// right after it is made, as VPI value change callbacks see them
    /// (IEEE 1800-2023 38.36.1). Other updates of them are shown when the
    /// unit that makes them ends. Only processes that write such a signal
    /// change, and only around those writes. Needs the native backend
    /// and [`Self::observable_updates`]; elsewhere, and in a tiered
    /// build, whose interpreted tier has no host context in its units,
    /// the points are dropped.
    pub fn value_change_signals<S: Into<String>>(
        mut self,
        paths: impl IntoIterator<Item = S>,
    ) -> Self {
        self.options.optimize_options = self
            .options
            .optimize_options
            .clone()
            .with_value_change_objects(paths.into_iter().map(Into::into).collect());
        self
    }

    /// The paths given to [`Self::value_change_signals`], which a new
    /// optimizer profile keeps.
    fn watched_objects(&self) -> Vec<String> {
        self.options
            .optimize_options
            .value_change_objects()
            .to_vec()
    }

    fn enforce_native_force_optimizer(&mut self) {
        if !self.options.native_force_support {
            return;
        }
        let diagnostics = self.options.optimize_options.diagnostics.clone();
        let watched = self.watched_objects();
        self.options.optimize_options =
            crate::optimizer::OptimizeOptions::none().with_value_change_objects(watched);
        self.options.optimize_options.diagnostics = diagnostics;
        self.options.dead_store_policy = DeadStorePolicy::Off;
    }

    /// Tell the frontend which state the host observes, so it can take
    /// the other combinational nets out of the program
    /// ([`crate::SirPass::NetInlining`]). The dead-store policy names
    /// that state, unless something else selects signals by name or at
    /// run time: a waveform, a live or forced signal, a watched object.
    fn declare_unobserved_nets(&mut self) {
        let named = self.vcd_path.is_some()
            || !self.live_signals.is_empty()
            || !self.options.signal_access.live_signals.is_empty()
            || !self.options.signal_access.force_signals.is_empty()
            || !self
                .options
                .optimize_options
                .value_change_objects()
                .is_empty();
        let policy = if named {
            DeadStorePolicy::Off
        } else {
            self.options.dead_store_policy
        };
        self.options.optimize_options = self
            .options
            .optimize_options
            .clone()
            .with_unobserved_nets(policy);
    }

    /// Set the overall optimization level. Sets defaults for SIR passes,
    /// Cranelift options, and DSE policy. Per-pass overrides can be applied after.
    pub fn opt_level(mut self, level: crate::optimizer::OptLevel) -> Self {
        let lanes = self.options.optimize_options.parallel_lanes();
        let partition = self.options.optimize_options.parallel_partition();
        let settle = self.options.optimize_options.settle_partition();
        let watched = self.watched_objects();
        self.options.optimize_options = crate::optimizer::OptimizeOptions::new(level)
            .with_parallel_lanes(lanes)
            .with_parallel_partition(partition)
            .with_settle_partition(settle)
            .with_value_change_objects(watched);
        self.options.cranelift_options = crate::backend::CraneliftOptions::for_speed_optimization(
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
        let lanes = self.options.optimize_options.parallel_lanes();
        let partition = self.options.optimize_options.parallel_partition();
        let settle = self.options.optimize_options.settle_partition();
        let watched = self.watched_objects();
        self.options.optimize_options = if enable {
            crate::optimizer::OptimizeOptions::all()
        } else {
            crate::optimizer::OptimizeOptions::none()
        }
        .with_parallel_lanes(lanes)
        .with_parallel_partition(partition)
        .with_settle_partition(settle)
        .with_value_change_objects(watched);
        self
    }

    /// Select the loop that runs time slots. A test option:
    /// For timed simulations, [`crate::SchedulerMode::Differential`]
    /// checks every step against a shadow simulation. Veryl TB execution
    /// uses this option to select compiled or reference clock batches.
    #[doc(hidden)]
    pub fn scheduler_mode(mut self, mode: crate::SchedulerMode) -> Self {
        self.options.scheduler = mode;
        self
    }

    /// Set per-pass optimizer flags directly.
    pub fn optimize_options(mut self, options: crate::optimizer::OptimizeOptions) -> Self {
        self.options.cranelift_options.tail_call_split =
            options.is_enabled(crate::optimizer::SirPass::TailCallSplit);
        // A thread count or partition mode requested separately survives
        // an options reset.
        let lanes = if options.parallel_lanes() > 1 {
            options.parallel_lanes()
        } else {
            self.options.optimize_options.parallel_lanes()
        };
        let partition = if options.parallel_partition() == crate::ParallelPartition::Always {
            crate::ParallelPartition::Always
        } else {
            self.options.optimize_options.parallel_partition()
        };
        let settle = if options.settle_partition() == crate::SettlePartition::Always {
            crate::SettlePartition::Always
        } else {
            self.options.optimize_options.settle_partition()
        };
        let watched = if options.value_change_objects().is_empty() {
            self.watched_objects()
        } else {
            options.value_change_objects().to_vec()
        };
        self.options.optimize_options = options
            .with_parallel_lanes(lanes)
            .with_parallel_partition(partition)
            .with_settle_partition(settle)
            .with_value_change_objects(watched);
        self
    }

    /// Evaluate the design on up to `threads` worker threads, the calling
    /// thread included.
    ///
    /// Compilation partitions the combinational settle and the sequential
    /// update of each clock event into one lane per thread, using the
    /// scheduler's dependency graph. Every state object written by a lane
    /// lives in that lane's own memory segment. A phase whose estimated
    /// speedup is too small keeps its sequential kernel, and so does every
    /// phase on backends without parallel execution. `1` (the default)
    /// compiles only the sequential kernels. Native force support keeps
    /// sequential execution.
    pub fn threads(mut self, threads: usize) -> Self {
        let lanes = if self.options.native_force_support {
            1
        } else {
            u32::try_from(threads)
                .unwrap_or(u32::MAX)
                .clamp(1, crate::MAX_SIMULATION_THREADS)
        };
        self.options.optimize_options.set_parallel_lanes(lanes);
        self
    }

    /// Choose how [`Self::threads`] decides which phases to partition.
    ///
    /// The default partitions a phase only when its estimated speedup
    /// outweighs lane synchronization. [`crate::ParallelPartition::Always`]
    /// partitions every phase with independent work; it exists for
    /// testing and measurement.
    pub fn parallel_partition(mut self, partition: crate::ParallelPartition) -> Self {
        self.options.optimize_options = self
            .options
            .optimize_options
            .clone()
            .with_parallel_partition(partition);
        self
    }

    /// Debug cross-check of a partitioned settle: the native and
    /// Cranelift backends run each settle a second time from the same
    /// state with every partition flagged, and panic at the first state
    /// byte the two runs leave differently. Slow; for testing the
    /// partitioning. Effects of the partitions that run in both passes,
    /// such as runtime events, happen twice.
    pub fn settle_check(mut self, enable: bool) -> Self {
        self.options.settle_check = enable;
        self
    }

    /// Cut the combinational settle into partitions that run only when
    /// state they read changed ([`crate::SirPass::PartitionedSettle`]).
    ///
    /// [`crate::SettlePartition::Estimated`] partitions a large settle of
    /// a design with several clock events;
    /// [`crate::SettlePartition::Always`] partitions every settle it can,
    /// for testing.
    /// Count the work of a partitioned settle: each partition's runs,
    /// the runs that changed something it compares, and the settles.
    /// The counters live at the end of the settle's flags; read them
    /// with `settle_profile` on the simulation. A run changing nothing
    /// is work the settle could have skipped.
    pub fn settle_profile(mut self, enable: bool) -> Self {
        self.options.optimize_options = self
            .options
            .optimize_options
            .clone()
            .with_settle_profile(enable);
        self
    }

    pub fn settle_partition(mut self, partition: crate::SettlePartition) -> Self {
        self.options.optimize_options = self
            .options
            .optimize_options
            .clone()
            .enable(crate::optimizer::SirPass::PartitionedSettle)
            .with_settle_partition(partition);
        self
    }

    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub fn x86_slp(mut self, enable: bool) -> Self {
        self.options.x86_options.slp = enable;
        self
    }

    #[cfg(not(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )))]
    pub fn x86_slp(self, enable: bool) -> Self {
        let _ = enable;
        self
    }

    /// Compile native code with the baseline pipeline that tiered
    /// execution uses for its first native image: lighter MIR
    /// optimization and the linear register allocator.
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub fn x86_baseline(mut self, enable: bool) -> Self {
        self.options.x86_options.baseline = enable;
        self
    }

    #[cfg(not(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )))]
    pub fn x86_baseline(self, enable: bool) -> Self {
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

    /// Link DPI-C imports to the C functions of a shared library.
    ///
    /// Libraries are searched in the order they are added, after the
    /// functions registered with [`Self::dpi_function`]. A library stays
    /// loaded for the rest of the process.
    ///
    /// # Safety
    ///
    /// Loading the library runs its initializers. Every C function a
    /// DPI-C import is linked to must have the signature that import
    /// declares (IEEE 1800-2023 35.5.6).
    pub unsafe fn dpi_library(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.options.dpi.add_library(path.into());
        self
    }

    /// Link the DPI-C import whose C name is `name` to `function`.
    ///
    /// Registering `name` again replaces the earlier function.
    ///
    /// # Safety
    ///
    /// `function` must be a C function with the signature its DPI-C
    /// import declares (IEEE 1800-2023 35.5.6), and must stay valid while
    /// any simulator built from this builder exists.
    pub unsafe fn dpi_function(mut self, name: impl Into<String>, function: *const ()) -> Self {
        self.options
            .dpi
            .add_function(name.into(), function as usize);
        self
    }

    /// Set the dead store elimination policy.
    pub fn dead_store_policy(mut self, policy: DeadStorePolicy) -> Self {
        self.options.dead_store_policy = policy;
        self
    }

    /// Set when the tiered backend adopts background-compiled code.
    pub fn tier_promotion(mut self, policy: TierPromotion) -> Self {
        self.options.tier_promotion = policy;
        self
    }

    /// Select incremental (hotspot) compilation on the tiered backend, the
    /// default, or whole-image tiered compilation with `false`.
    /// Unsupported configurations retain whole-image tiering.
    pub fn hotspot_jit(mut self, enabled: bool) -> Self {
        self.options.hotspot_jit = enabled;
        self
    }

    /// Declare hierarchical observation and native force targets.
    /// This preserves the requested optimization level. Force targets currently
    /// require a pure SystemVerilog design and the native backend.
    pub fn signal_access(mut self, options: crate::SignalAccessOptions) -> Self {
        self.options.signal_access = options;
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

    /// Use legacy zero results for all empty SV array reductions.
    /// By default product returns one and bitwise AND returns all ones.
    pub fn legacy_empty_reductions(mut self, enable: bool) -> Self {
        self.options.legacy_empty_reductions = enable;
        self
    }

    /// Set default SV timing and contribute its precision to the scheduler.
    /// Explicit source time declarations retain their units and rounding.
    pub fn sv_timescale(mut self, timescale: Option<crate::SvTimescale>) -> Self {
        self.options.sv_timescale = timescale;
        self
    }

    /// Override recoverable compilation diagnostic severities.
    pub fn diagnostic_policy(mut self, policy: crate::DiagnosticPolicy) -> Self {
        self.options.diagnostic_policy = policy;
        self
    }

    /// Configure runtime and optimizer diagnostics explicitly for this build.
    pub fn diagnostics(mut self, diagnostics: crate::DiagnosticsOptions) -> Self {
        // A switch the builder set stays set, whatever order the calls
        // come in.
        let force_out_of_line = self.options.diagnostics.sv_force_out_of_line;
        self.options.diagnostics = diagnostics.runtime;
        self.options.diagnostics.sv_force_out_of_line |= force_out_of_line;
        self.options.optimize_options.diagnostics = diagnostics.sir;
        self.options.cranelift_options.diagnostics = diagnostics.cranelift;
        #[cfg(any(
            target_arch = "x86_64",
            feature = "arm64-codegen",
            target_arch = "aarch64"
        ))]
        {
            self.options.x86_options.diagnostics = diagnostics.native;
            if let Some(enabled) = diagnostics.native_tick_loop {
                self.options.x86_options.native_tick_loop = enabled;
            }
        }
        self
    }

    /// Lower every SystemVerilog subroutine that can be into a function
    /// unit and call it, instead of inlining it at each call: a test
    /// switch for the out-of-line call path, which leaves the
    /// subroutines that cannot be lowered so inlined (see
    /// `RuntimeDiagnostics::sv_force_out_of_line`).
    #[doc(hidden)]
    pub fn force_out_of_line_calls(mut self) -> Self {
        self.options.diagnostics.sv_force_out_of_line = true;
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

    /// Record the partitioned settle's plan with a lint of its
    /// dependencies ([`crate::CompilationTrace::settle_plan`]).
    pub fn trace_settle_plan(mut self) -> Self {
        self.options.trace.settle_plan = true;
        self
    }

    /// Write the partitioned settle's plan, with a lint of its
    /// dependencies ([`crate::CompilationTrace::settle_plan`]), to
    /// `path` while building. Nothing is written without a partitioned
    /// settle.
    pub fn settle_plan_output(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.options.trace.settle_plan_output = Some(path.into());
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
    /// analysis. The analysis is captured by [`Self::build_with_trace`] or
    /// [`Self::compile_native_with_trace`] from the exact merged SIR passed
    /// to native instruction selection.
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

/// The stages after the frontend on programs whose units call function
/// units. No frontend makes calls yet, so the tests inject them into the
/// SIR of ordinary designs: each design gives the objects it involves
/// distinct widths, by which the injection finds them.
#[cfg(test)]
mod call_consumer_tests;
