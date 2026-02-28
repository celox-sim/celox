use veryl_analyzer::{Analyzer, AnalyzerError, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::Parser;

use super::Simulator;
use crate::{ParserError, SimulatorError, backend::JitBackend, ir::Program, parser};
fn analyze(
    code: &str,
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
    optimize: bool,
    trace_opts: &crate::debug::TraceOptions,
    trace_out: Option<&mut crate::debug::CompilationTrace>,
    metadata: Option<Metadata>,
) -> (Result<Program, ParserError>, Vec<AnalyzerError>) {
    symbol_table::clear();
    attribute_table::clear();

    let metadata = metadata.unwrap_or_else(|| Metadata::create_default("prj").unwrap());
    let parser = Parser::parse(code, &"").unwrap();
    let analyzer = Analyzer::new(&metadata);
    let mut context = Context::default();

    let mut ir = Ir::default();

    let mut errors = vec![];
    errors.append(&mut analyzer.analyze_pass1("prj", &parser.veryl));
    errors.append(&mut Analyzer::analyze_post_pass1());
    errors.append(&mut analyzer.analyze_pass2("prj", &parser.veryl, &mut context, Some(&mut ir)));

    errors.append(&mut Analyzer::analyze_post_pass2());
    let errors: Vec<_> = errors
        .into_iter()
        .filter(|x| matches!(x, AnalyzerError::UnsupportedByIr { .. }))
        .collect();
    let top = veryl_parser::resource_table::insert_str(top);
    let sir = parser::parse(
        &top,
        &ir,
        optimize,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
    );
    (sir, errors)
}
pub(crate) fn compile_to_sir(
    code: &str,
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
    optimize: bool,
    trace_opts: &crate::debug::TraceOptions,
    trace_out: Option<&mut crate::debug::CompilationTrace>,
    metadata: Option<Metadata>,
) -> Result<Program, ParserError> {
    let (sir, errors) = analyze(
        code,
        top,
        ignored_loops,
        true_loops,
        four_state,
        optimize,
        trace_opts,
        trace_out,
        metadata,
    );
    if !errors.is_empty() {
        panic!("Compiler errors found: {:?}", errors);
    }
    sir
}
#[derive(Debug, Clone)]
pub struct SimulatorOptions {
    pub four_state: bool,
    pub optimize: bool,
    pub trace: crate::debug::TraceOptions,
    /// When true, JIT-compiled functions emit trigger detection code for
    /// edge-based event discovery. Only needed by [`crate::Simulation`].
    pub emit_triggers: bool,
}

impl Default for SimulatorOptions {
    fn default() -> Self {
        Self {
            four_state: false,
            optimize: true,
            trace: Default::default(),
            emit_triggers: false,
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
    code: &'a str,
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
    _marker: std::marker::PhantomData<Target>,
}

/// Configuration methods shared by all builder variants.
impl<'a, Target> SimulatorBuilder<'a, Target> {
    /// Supply project metadata (clock/reset settings, etc.) instead of defaults.
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = Some(metadata);
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

    /// Enable or disable SIRT optimization passes.
    pub fn optimize(mut self, enable: bool) -> Self {
        self.options.optimize = enable;
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

    pub fn trace_on_build(mut self) -> Self {
        self.options.trace.output_to_stdout = true;
        self
    }

    /// Explicitly ignore a dependency between two signals.
    ///
    /// Use this to break "false loops" where a combinational cycle exists
    /// structurally but never occurs logically during execution.
    pub fn false_loop(
        mut self,
        from: (Vec<(String, usize)>, Vec<String>),
        to: (Vec<(String, usize)>, Vec<String>),
    ) -> Self {
        self.ignored_loops.push((from, to));
        self
    }
    /// Mark a dependency as a "true loop" and specify its convergence limit.
    ///
    /// The simulator will use a convergence-based repetition strategy (Dynamic Convergence)
    /// to stabilize the combinational logic within this loop up to `max_iter` times.
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
            code,
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            _marker: std::marker::PhantomData,
        }
    }

    /// Compiles the Veryl source and constructs the core logic simulator.
    pub fn build(self) -> Result<Simulator, SimulatorError> {
        let program = compile_to_sir(
            self.code,
            self.top,
            &self.ignored_loops,
            &self.true_loops,
            self.options.four_state,
            self.options.optimize,
            &self.options.trace,
            None,
            self.metadata,
        )
        .map_err(SimulatorError::SIRParser)?;
        let backend = JitBackend::new(&program, &self.options, None)?;

        let mut sim = Simulator::with_backend_and_program(backend, program);
        if let Some(path) = self.vcd_path {
            let vcd_writer = crate::vcd::VcdWriter::new(path, &sim.program)
                .map_err(|_| SimulatorError::Runtime(crate::RuntimeErrorCode::InternalError))?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.modify(|_| {}).map_err(SimulatorError::Runtime)?;
        Ok(sim)
    }

    /// Compiles the Veryl source and constructs the core logic simulator,
    /// while capturing compilation trace data as configured by TraceOptions.
    pub fn build_with_trace(self) -> crate::debug::CompilationTraceResult {
        let mut trace = crate::debug::CompilationTrace::default();
        let program_res = compile_to_sir(
            self.code,
            self.top,
            &self.ignored_loops,
            &self.true_loops,
            self.options.four_state,
            self.options.optimize,
            &self.options.trace,
            Some(&mut trace),
            self.metadata,
        )
        .map_err(SimulatorError::SIRParser);

        let sim_res = program_res.and_then(|program| {
            let backend = JitBackend::new(&program, &self.options, Some(&mut trace))?;

            let mut sim = Simulator::with_backend_and_program(backend, program);
            sim.modify(|_| {}).map_err(SimulatorError::Runtime)?;
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

impl<'a> SimulatorBuilder<'a, crate::Simulation> {
    pub(crate) fn new(code: &'a str, top: &'a str) -> Self {
        Self {
            code,
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            _marker: std::marker::PhantomData,
        }
    }

    /// Compiles the Veryl source and constructs the timed simulation wrapper.
    pub fn build(mut self) -> Result<crate::Simulation, SimulatorError> {
        self.options.emit_triggers = true;
        let program = compile_to_sir(
            self.code,
            self.top,
            &self.ignored_loops,
            &self.true_loops,
            self.options.four_state,
            self.options.optimize,
            &self.options.trace,
            None,
            self.metadata,
        )
        .map_err(SimulatorError::SIRParser)?;
        let backend = JitBackend::new(&program, &self.options, None)?;

        let mut sim = Simulator::with_backend_and_program(backend, program);
        if let Some(path) = self.vcd_path {
            let vcd_writer = crate::vcd::VcdWriter::new(path, &sim.program)
                .map_err(|_| SimulatorError::Runtime(crate::RuntimeErrorCode::InternalError))?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.modify(|_| {}).map_err(SimulatorError::Runtime)?;
        Ok(crate::Simulation::new(sim))
    }
}
