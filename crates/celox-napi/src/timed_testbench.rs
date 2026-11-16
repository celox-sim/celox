//! Running a timed testbench (a design whose top contains timed
//! processes, such as a SystemVerilog testbench) on the interpreter
//! backend, on both native and wasm32 hosts.
//!
//! The playground's TypeScript runtime only executes combinational and
//! flip-flop kernels, so a testbench that generates its own clock and
//! checks results in `initial` blocks needs this engine: the schedule of
//! [`celox_runtime::SimulationState`] driven by [`celox::InterpBackend`].

use super::*;
use celox::SimBackend as _;
use celox_runtime::{Halt, HostBoundary, ResolvedSchedule, SimulationState, UnitExecutor};

/// Minimal executor: every unit runs on the interpreter backend and no
/// host boundary work happens between units.
struct InterpExecutor {
    backend: celox::InterpBackend,
}

impl UnitExecutor for InterpExecutor {
    type Backend = celox::InterpBackend;

    fn backend(&self) -> &Self::Backend {
        &self.backend
    }

    fn backend_mut(&mut self) -> &mut Self::Backend {
        &mut self.backend
    }

    fn eval_comb(&mut self) -> std::result::Result<(), Halt> {
        Ok(self.backend.eval_comb()?)
    }

    fn eval_apply_ff_at(
        &mut self,
        event: <Self::Backend as celox::SimBackend>::Event,
    ) -> std::result::Result<(), Halt> {
        Ok(self.backend.eval_apply_ff_at(event)?)
    }

    fn eval_only_ff_at(
        &mut self,
        event: <Self::Backend as celox::SimBackend>::Event,
    ) -> std::result::Result<(), Halt> {
        Ok(self.backend.eval_only_ff_at(event)?)
    }

    fn apply_ff_at(
        &mut self,
        event: <Self::Backend as celox::SimBackend>::Event,
    ) -> std::result::Result<(), Halt> {
        Ok(self.backend.apply_ff_at(event)?)
    }

    fn run_process(&mut self, index: usize) -> std::result::Result<(), Halt> {
        Ok(self.backend.run_process(index)?)
    }
}

impl HostBoundary for InterpExecutor {}

/// Compile `sources` and run the timed testbench rooted at `top` until it
/// finishes (`$finish`), runs out of work, fails, or reaches `max_steps`
/// scheduler steps. Returns a JSON object:
///
/// `{ "passed": bool, "finished": bool, "time": number,
///    "signals": { "<path>": "<decimal value>", ... },
///    "error": string | null }`
///
/// `passed` means the run finished without a scheduler error; a testbench
/// reports its own check results through its signals (for example an
/// `errors` counter it maintains), which the caller inspects.
#[napi]
pub fn run_timed_testbench(
    sources: Vec<NapiSourceFile>,
    top: String,
    max_steps: Option<u32>,
) -> Result<String> {
    let src_pairs: Vec<(String, std::path::PathBuf)> = sources
        .into_iter()
        .map(|s| (s.content, std::path::PathBuf::from(s.path)))
        .collect();
    let (veryl_pairs, sv_pairs) = partition_hdl_sources(&src_pairs);
    let veryl_refs: Vec<(&str, &std::path::Path)> = veryl_pairs
        .iter()
        .map(|(s, p)| (s.as_str(), p.as_path()))
        .collect();
    let sv_refs: Vec<(&str, &std::path::Path)> = sv_pairs
        .iter()
        .map(|(s, p)| (s.as_str(), p.as_path()))
        .collect();

    let trace_opts = celox::TraceOptions::default();
    let (program, _warnings) = match (veryl_refs.is_empty(), sv_refs.is_empty()) {
        (false, true) | (true, true) => celox::compile_to_sir_with_diagnostic_policy(
            &veryl_refs,
            &top,
            &[],
            &[],
            false,
            &trace_opts,
            None,
            None,
            None,
            None,
            &[],
            &celox::OptimizeOptions::default(),
            &celox::DiagnosticPolicy::default(),
            &Default::default(),
        ),
        (true, false) => celox::compile_sv_to_sir(
            &sv_refs,
            &top,
            &[],
            &[],
            false,
            &trace_opts,
            None,
            None,
            None,
            None,
            &[],
            &celox::OptimizeOptions::default(),
        ),
        (false, false) => celox::compile_mixed_to_sir(
            &veryl_refs,
            &sv_refs,
            &top,
            &[],
            &[],
            false,
            &trace_opts,
            None,
            None,
            None,
            None,
            &[],
            &celox::OptimizeOptions::default(),
        ),
    }
    .map_err(|e| Error::from_reason(format!("{e}")))?;

    let laid_out = program.into_laid_out_with_mode(false, celox::MemoryLayoutMode::ElementStrided);
    let backend = celox::InterpBackend::new_with_modes(&laid_out, false, true)
        .map_err(|e| Error::from_reason(format!("{e}")))?;
    let schedule =
        celox_design::schedule::ScheduleModel::derive(&laid_out.design, &laid_out.runtime_schema);
    let mut state = SimulationState::new(&backend, ResolvedSchedule::resolve(&backend, &schedule));
    state.set_process_names(
        laid_out
            .runtime_schema
            .process_origins
            .iter()
            .map(ToString::to_string)
            .collect(),
    );
    let mut executor = InterpExecutor { backend };

    let max_steps = u64::from(max_steps.unwrap_or(1_000_000));
    let mut error: Option<String> = None;
    let mut steps = 0u64;
    loop {
        if steps >= max_steps {
            error = Some(format!("step limit of {max_steps} reached"));
            break;
        }
        match state.step(&mut executor) {
            Ok(Some(_)) => steps += 1,
            Ok(None) => break,
            Err(e) => {
                error = Some(format!("{e:?}"));
                break;
            }
        }
        if state.is_finished() {
            break;
        }
    }
    let finished = state.is_finished();
    let time = state.time();

    // Read every named signal of the design for the caller to inspect.
    let mut signals = serde_json::Map::new();
    for addr in laid_out.design.state_objects.keys() {
        let Some(variable) = laid_out.design.variable(addr) else {
            continue;
        };
        let Some(instance) = laid_out.design.instance(addr.instance_id) else {
            continue;
        };
        if !instance.resolves_path_to(&variable.path, *addr) {
            continue;
        }
        let name = laid_out.get_path(addr);
        let value = executor.backend.get(executor.backend.resolve_signal(addr));
        signals.insert(name, serde_json::Value::String(value.to_string()));
    }

    let output = serde_json::json!({
        "passed": finished && error.is_none(),
        "finished": finished,
        "time": time,
        "signals": signals,
        "error": error,
    });
    serde_json::to_string(&output)
        .map_err(|e| Error::from_reason(format!("Failed to serialize result: {e}")))
}
