use std::path::Path;

use veryl_analyzer::ir::{Comptime, Expression, VarPath};
use veryl_analyzer::value::Value;
use veryl_analyzer::{Analyzer, AnalyzerError, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::{ClockType, Metadata, ResetType};
use veryl_parser::Parser;
use veryl_parser::resource_table;

use crate::parser::BuildConfig;
use crate::{ParserError, SimulatorError, SimulatorErrorKind, ir::Program, parser};

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
) -> (Result<Program, ParserError>, Vec<AnalyzerError>) {
    symbol_table::clear();
    attribute_table::clear();

    let metadata = metadata.unwrap_or_else(|| Metadata::create_default("prj").unwrap());
    let analyzer = Analyzer::new(&metadata);

    // Per-file: parse + pass1
    let mut parsers = Vec::new();
    let mut errors = vec![];
    for (code, path) in sources {
        let parsed = Parser::parse(code, path).unwrap();
        errors.append(&mut analyzer.analyze_pass1("prj", &parsed.veryl));
        parsers.push(parsed);
    }

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
        errors.append(&mut analyzer.analyze_pass2(
            "prj",
            &parsed.veryl,
            &mut context,
            Some(&mut ir),
        ));
    }
    errors.append(&mut Analyzer::analyze_post_pass2());

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
        &build_config,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        optimize_options,
    );
    (sir, errors)
}

/// Compile Veryl source code to the SIR (Simulation IR) representation.
///
/// This is the shared compilation pipeline used by all backends.
/// Returns the compiled Program and any analyzer warnings on success.
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
) -> Result<(Program, Vec<AnalyzerError>), SimulatorError> {
    let (sir, errors) = analyze(
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
    );
    let (real_errors, warnings): (Vec<_>, Vec<_>) = errors.into_iter().partition(|e| e.is_error());
    if !real_errors.is_empty() {
        return Err(
            SimulatorError::new(SimulatorErrorKind::Analyzer(real_errors)).with_warnings(warnings),
        );
    }
    match sir {
        Ok(p) => Ok((p, warnings)),
        Err(e) => Err(SimulatorError::from(e).with_warnings(warnings)),
    }
}

// ── JIT-specific types and builders (native only) ────────────────────

#[cfg(not(target_arch = "wasm32"))]
use super::Simulator;
#[cfg(not(target_arch = "wasm32"))]
use crate::backend::JitBackend;

/// Controls which stores the dead store elimination pass preserves.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeadStorePolicy {
    /// Keep all stores (no dead store elimination). Default for user-facing builds.
    #[default]
    Off,
    /// Eliminate stores except those to top-module ports and those loaded by EUs.
    PreserveTopPorts,
    /// Eliminate stores except those to ports of *all* instances and those loaded by EUs.
    PreserveAllPorts,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct SimulatorOptions {
    pub four_state: bool,
    /// Per-pass SIRT optimizer flags.
    pub optimize_options: crate::optimizer::OptimizeOptions,
    /// Cranelift backend optimization level.
    pub cranelift_opt_level: crate::optimizer::CraneliftOptLevel,
    /// Fine-grained Cranelift backend options.
    pub cranelift_options: crate::optimizer::CraneliftOptions,
    pub trace: crate::debug::TraceOptions,
    /// When true, JIT-compiled functions emit trigger detection code for
    /// edge-based event discovery. Only needed by [`crate::Simulation`].
    pub emit_triggers: bool,
    /// Dead store elimination policy.
    pub dead_store_policy: DeadStorePolicy,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for SimulatorOptions {
    fn default() -> Self {
        Self {
            four_state: false,
            optimize_options: crate::optimizer::OptimizeOptions::default(),
            cranelift_opt_level: crate::optimizer::CraneliftOptLevel::default(),
            cranelift_options: crate::optimizer::CraneliftOptions::default(),
            trace: Default::default(),
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
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
impl<'a, Target> SimulatorBuilder<'a, Target> {
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

    /// Enable or disable all SIRT optimization passes at once.
    pub fn optimize(mut self, enable: bool) -> Self {
        self.options.optimize_options = if enable {
            crate::optimizer::OptimizeOptions::all()
        } else {
            crate::optimizer::OptimizeOptions::none()
        };
        self
    }

    /// Set per-pass optimizer flags.
    pub fn optimize_options(mut self, options: crate::optimizer::OptimizeOptions) -> Self {
        self.options.optimize_options = options;
        self
    }

    /// Set the Cranelift backend optimization level.
    pub fn cranelift_opt_level(mut self, level: crate::optimizer::CraneliftOptLevel) -> Self {
        self.options.cranelift_opt_level = level;
        self.options.cranelift_options.opt_level = level;
        self
    }

    /// Set fine-grained Cranelift backend options.
    pub fn cranelift_options(mut self, options: crate::optimizer::CraneliftOptions) -> Self {
        self.options.cranelift_options = options;
        self.options.cranelift_opt_level = options.opt_level;
        self
    }

    /// Set the register allocator algorithm.
    pub fn regalloc_algorithm(mut self, algo: crate::optimizer::RegallocAlgorithm) -> Self {
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

    /// Enable or disable the store-load forwarding pass.
    pub fn store_load_forwarding(mut self, enable: bool) -> Self {
        self.options.optimize_options.store_load_forwarding = enable;
        self
    }

    /// Enable or disable the hoist-common-branch-loads pass.
    pub fn hoist_common_branch_loads(mut self, enable: bool) -> Self {
        self.options.optimize_options.hoist_common_branch_loads = enable;
        self
    }

    /// Enable or disable the bit-extract peephole pass.
    pub fn bit_extract_peephole(mut self, enable: bool) -> Self {
        self.options.optimize_options.bit_extract_peephole = enable;
        self
    }

    /// Enable or disable the block optimization pass.
    pub fn optimize_blocks(mut self, enable: bool) -> Self {
        self.options.optimize_options.optimize_blocks = enable;
        self
    }

    /// Enable or disable the split-wide-commits pass.
    pub fn split_wide_commits(mut self, enable: bool) -> Self {
        self.options.optimize_options.split_wide_commits = enable;
        self
    }

    /// Enable or disable the commit sinking pass.
    pub fn commit_sinking(mut self, enable: bool) -> Self {
        self.options.optimize_options.commit_sinking = enable;
        self
    }

    /// Enable or disable the inline commit forwarding pass.
    pub fn inline_commit_forwarding(mut self, enable: bool) -> Self {
        self.options.optimize_options.inline_commit_forwarding = enable;
        self
    }

    /// Enable or disable the dead working stores elimination pass.
    pub fn eliminate_dead_working_stores(mut self, enable: bool) -> Self {
        self.options.optimize_options.eliminate_dead_working_stores = enable;
        self
    }

    /// Enable or disable the reschedule pass.
    pub fn reschedule(mut self, enable: bool) -> Self {
        self.options.optimize_options.reschedule = enable;
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

#[cfg(not(target_arch = "wasm32"))]
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

    /// Compile SIR and return it along with the remaining builder state.
    /// Consumes self.
    fn into_sir(
        self,
    ) -> Result<
        (
            crate::ir::Program,
            Vec<veryl_analyzer::AnalyzerError>,
            SimulatorOptions,
            Option<std::path::PathBuf>,
        ),
        SimulatorError,
    > {
        let (mut program, warnings) = compile_to_sir(
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
        )?;

        if self.options.dead_store_policy != DeadStorePolicy::Off {
            run_dead_store_elimination(
                &mut program,
                &self.live_signals,
                self.options.dead_store_policy,
            );
        }

        Ok((program, warnings, self.options, self.vcd_path))
    }

    /// Compiles the Veryl source and constructs the simulator.
    /// Uses the native x86-64 backend.
    pub fn build(
        self,
    ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
        self.build_native()
    }

    /// Compiles using the Cranelift JIT backend.
    pub fn build_cranelift(self) -> Result<Simulator<JitBackend>, SimulatorError> {
        let phase_timing = std::env::var("CELOX_PHASE_TIMING").is_ok();
        let phase_start = phase_timing.then(crate::timing::now);

        let (program, warnings, options, vcd_path) = self.into_sir()?;

        if let Some(s) = phase_start {
            eprintln!("[phase-timing] compile_to_sir (total): {:?}", s.elapsed());
        }

        let jit_start = phase_timing.then(crate::timing::now);
        let backend = JitBackend::new(&program, &options, None)?;
        if let Some(s) = jit_start {
            eprintln!("[phase-timing] jit_backend: {:?}", s.elapsed());
        }

        let mut sim = Simulator::with_backend_and_program(backend, program, warnings);
        if let Some(path) = vcd_path {
            let descs = sim.build_vcd_descs(options.four_state);
            let vcd_writer = crate::vcd::VcdWriter::new(path, &descs)
                .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.modify(|_| {}).map_err(SimulatorError::from)?;
        Ok(sim)
    }

    /// Compiles using the native x86-64 backend.
    pub fn build_native(
        self,
    ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
        let (program, warnings, options, _vcd_path) = self.into_sir()?;
        let backend = crate::backend::native::NativeBackend::new(&program, &options)?;
        let mut sim = Simulator::with_backend_and_program(backend, program, warnings);
        sim.modify(|_| {}).map_err(SimulatorError::from)?;
        Ok(sim)
    }

    /// Compiles the Veryl source and constructs the core logic simulator,
    /// while capturing compilation trace data as configured by TraceOptions.
    pub fn build_with_trace(self) -> crate::debug::CompilationTraceResult {
        let mut trace = crate::debug::CompilationTrace::default();
        let program_res = compile_to_sir(
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
        );

        let sim_res = program_res.and_then(|(mut program, warnings)| {
            if self.options.dead_store_policy != DeadStorePolicy::Off {
                run_dead_store_elimination(
                    &mut program,
                    &self.live_signals,
                    self.options.dead_store_policy,
                );
            }

            // Run MIR trace if requested (generates MIR output before/after regalloc)
            if self.options.trace.mir {
                use crate::backend::native::{emit, isel, regalloc};
                let layout = crate::backend::MemoryLayout::build(&program, false);
                let mut mir_output = String::new();

                mir_output.push_str("=== MIR (eval_comb) ===\n");
                for (idx, eu) in program.eval_comb.iter().enumerate() {
                    let mut mfunc = isel::lower_execution_unit(eu, &layout);
                    mir_output.push_str(&format!("Execution Unit {idx} (before regalloc):\n"));
                    mir_output.push_str(&format!("{mfunc}\n"));
                    let assignment = regalloc::run_regalloc(&mut mfunc);
                    mir_output.push_str(&format!("Execution Unit {idx} (after regalloc):\n"));
                    mir_output.push_str(&format!("{mfunc}"));
                    mir_output.push_str("  Register assignment:\n");
                    for (vreg, preg) in &assignment.map {
                        mir_output.push_str(&format!("    {vreg} -> {preg}\n"));
                    }
                    if let Ok(result) = emit::emit(&mfunc, &assignment, 0) {
                        mir_output.push_str("  x86-64 disassembly:\n");
                        mir_output.push_str(&emit::disassemble(&result.code, 0));
                    }
                    mir_output.push('\n');
                }
                trace.mir = Some(mir_output);
            }

            let backend = crate::backend::native::NativeBackend::new(
                &program, &self.options,
            )?;

            let mut sim = Simulator::with_backend_and_program(backend, program, warnings);
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

#[cfg(not(target_arch = "wasm32"))]
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
        let (mut program, warnings) = compile_to_sir(
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
        )?;
        if self.options.dead_store_policy != DeadStorePolicy::Off {
            run_dead_store_elimination(
                &mut program,
                &self.live_signals,
                self.options.dead_store_policy,
            );
        }
        let backend = JitBackend::new(&program, &self.options, None)?;

        let mut sim = Simulator::with_backend_and_program(backend, program, warnings);
        if let Some(path) = self.vcd_path {
            let descs = sim.build_vcd_descs(self.options.four_state);
            let vcd_writer = crate::vcd::VcdWriter::new(path, &descs)
                .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.modify(|_| {}).map_err(SimulatorError::from)?;
        Ok(crate::Simulation::new(sim))
    }
}

/// Resolve user-specified `(instance_path, var_path)` to `AbsoluteAddr` and run DSE.
#[cfg(not(target_arch = "wasm32"))]
fn run_dead_store_elimination(
    program: &mut Program,
    live_signals: &[(Vec<(String, usize)>, Vec<String>)],
    policy: DeadStorePolicy,
) {
    use crate::HashSet;
    use crate::ir::{AbsoluteAddr, InstancePath};
    let mut externally_live = HashSet::default();

    // User-specified live signals
    for (inst_path, var_path) in live_signals {
        let inst_refs: Vec<(&str, usize)> =
            inst_path.iter().map(|(s, i)| (s.as_str(), *i)).collect();
        let var_refs: Vec<&str> = var_path.iter().map(|s| s.as_str()).collect();
        let addr = program.get_addr(&inst_refs, &var_refs).unwrap();
        externally_live.insert(addr);
    }

    // PreserveTopPorts: auto-collect top module port addresses
    if policy == DeadStorePolicy::PreserveTopPorts {
        if let Some(&top_instance_id) = program.instance_ids.get(&InstancePath(vec![])) {
            if let Some(&top_module_id) = program.instance_module.get(&top_instance_id) {
                if let Some(top_vars) = program.module_variables.get(&top_module_id) {
                    for info in top_vars.values() {
                        if info.var_kind.is_port() {
                            externally_live.insert(AbsoluteAddr {
                                instance_id: top_instance_id,
                                var_id: info.id,
                            });
                        }
                    }
                }
            }
        }
    }

    // PreserveAllPorts: collect port addresses from every instance
    if policy == DeadStorePolicy::PreserveAllPorts {
        for (&instance_id, &module_id) in &program.instance_module {
            if let Some(vars) = program.module_variables.get(&module_id) {
                for info in vars.values() {
                    if info.var_kind.is_port() {
                        externally_live.insert(AbsoluteAddr {
                            instance_id,
                            var_id: info.id,
                        });
                    }
                }
            }
        }
    }

    crate::optimizer::coalescing::pass_dead_store_elimination::eliminate_dead_stores(
        program,
        &externally_live,
    );
}
