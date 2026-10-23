//! Celox and reference adapters for the independent Veryl corpus.
#![allow(dead_code)]

use celox::{BigUint, ParserError, SimBackend, Simulator, SimulatorError, SimulatorErrorKind};
use celox_test_suite::{Backend, CompilationRejected, Design, Result, SignalPath};

struct CeloxBackend<B: SimBackend>(Simulator<B>);

impl<B: SimBackend> CeloxBackend<B> {
    fn signal(&self, path: &SignalPath) -> celox::SignalRef {
        let instances: Vec<_> = path
            .instances
            .iter()
            .map(|i| (i.name.as_str(), i.index.unwrap_or(0)))
            .collect();
        self.0.child_signal(&instances, &path.name)
    }
}

impl<B: SimBackend> Backend for CeloxBackend<B> {
    fn write(&mut self, path: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        let signal = self.signal(path);
        if mask == BigUint::default() {
            self.0.set_wide(signal, payload);
        } else {
            self.0.set_four_state(signal, payload, mask);
        }
        Ok(())
    }

    fn read(&mut self, path: &SignalPath) -> Result<(BigUint, BigUint)> {
        Ok(self.0.get_four_state(self.signal(path)))
    }

    fn eval_comb(&mut self) -> Result<()> {
        self.0.eval_comb().map_err(Into::into)
    }

    fn take_output(&mut self) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        for event in self.0.drain_runtime_events() {
            match event {
                celox::RuntimeEvent::Display { message } => {
                    output.extend_from_slice(&message);
                    output.push(b'\n');
                }
                celox::RuntimeEvent::Write { message } => output.extend_from_slice(&message),
                celox::RuntimeEvent::Missed { count } => {
                    return Err(format!("{count} runtime events were missed").into());
                }
                _ => (),
            }
        }
        Ok(output)
    }

    fn run_testbench(&mut self) -> Result<()> {
        let program = celox::testbench::compile_initial_testbench(&self.0)
            .ok_or("testbench program was not compiled")?;
        match celox::testbench::run_compiled_testbench_to_finish(&mut self.0, &program) {
            celox::TestResult::Pass => Ok(()),
            result => Err(format!("testbench failed: {result:?}").into()),
        }
    }

    fn tick(&mut self, event: &str) -> Result<()> {
        // The suite pulses the port as the external testbenches do: to its
        // inactive level, to its active level, and back unless it was active.
        let handle = self.0.event(event);
        let signal = self.0.signal(event);
        let active = self.0.event_edges()[celox::EventHandle::id(&handle)].active_edge();
        let (value, mask) = self.0.get_four_state(signal);
        let was_active = mask == BigUint::default()
            && (value != BigUint::default()) == (active == celox::Edge::Rising);
        self.0.inactive_edge(handle)?;
        self.0.active_edge(handle)?;
        if !was_active {
            self.0.inactive_edge(handle)?;
        }
        Ok(())
    }
}

struct VerylBackend(super::veryl_sim::VerylSimAdapter);

impl VerylBackend {
    fn signal(&mut self, path: &SignalPath) -> super::veryl_sim::VerylSignalRef {
        let instances: Vec<_> = path
            .instances
            .iter()
            .map(|i| (i.name.as_str(), i.index.unwrap_or(0)))
            .collect();
        self.0.child_signal(&instances, &path.name)
    }
}

impl Backend for VerylBackend {
    fn write(&mut self, path: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        let signal = self.signal(path);
        if mask == BigUint::default() {
            self.0.set_wide(signal, payload);
        } else {
            self.0.set_four_state(signal, payload, mask);
        }
        Ok(())
    }

    fn read(&mut self, path: &SignalPath) -> Result<(BigUint, BigUint)> {
        let signal = self.signal(path);
        Ok(self.0.get_four_state(signal))
    }

    fn eval_comb(&mut self) -> Result<()> {
        self.0.eval_comb().map_err(Into::into)
    }

    fn tick(&mut self, event: &str) -> Result<()> {
        let event = self.0.event(event);
        self.0.tick(event).map_err(Into::into)
    }
}

pub fn build(design: &Design, backend: &str) -> Result<Box<dyn Backend>> {
    let sources = design
        .sources
        .iter()
        .map(|source| (source.text.as_str(), source.path.as_path()))
        .collect::<Vec<_>>();
    if backend == "veryl" {
        if !design.parameters.is_empty() {
            return Err("the veryl-simulator adapter cannot set top parameters".into());
        }
        return Ok(Box::new(VerylBackend(
            super::veryl_sim::build_veryl_adapter(&sources, &design.top, design.four_state),
        )));
    }
    #[cfg(feature = "systemverilog")]
    if backend == "sv" {
        return build_sv(design, &sources);
    }
    let builder = design.parameters.iter().fold(
        Simulator::from_sources(sources, &design.top)
            .four_state(design.four_state)
            .allow_inferred_latches(design.allow_inferred_latches)
            .allow_always_ff_function_effects(true),
        |builder, (name, value)| builder.param(name, *value),
    );
    Ok(match backend {
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        "native" => Box::new(CeloxBackend(
            builder.build_native().map_err(classify_build_error)?,
        )),
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        "native-parallel" => Box::new(CeloxBackend(
            builder
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
                .build_native()
                .map_err(classify_build_error)?,
        )),
        "cranelift" => Box::new(CeloxBackend(
            builder.build_cranelift().map_err(classify_build_error)?,
        )),
        "cranelift-parallel" => Box::new(CeloxBackend(
            builder
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
                .build_cranelift()
                .map_err(classify_build_error)?,
        )),
        "wasm" => Box::new(CeloxBackend(
            builder.build_wasm().map_err(classify_build_error)?,
        )),
        "interp" => Box::new(CeloxBackend(
            builder.build_interpreter().map_err(classify_build_error)?,
        )),
        _ => return Err(format!("unknown suite backend: {backend}").into()),
    })
}

// Only source diagnostics satisfy a negative fixture. Codegen, IR verification,
// runtime, and unrelated unsupported-feature failures must fail the test.
pub fn classify_build_error(error: SimulatorError) -> celox_test_suite::Error {
    let rejected = matches!(
        error.kind(),
        SimulatorErrorKind::Analyzer(_)
            | SimulatorErrorKind::Frontend(_)
            | SimulatorErrorKind::SIRParser(
                ParserError::IllegalContext { .. }
                    | ParserError::InvalidFunctionArgumentBinding { .. }
                    | ParserError::Unsupported {
                        feature: "systemverilog output port lvalue connection",
                        ..
                    }
            )
    );
    if rejected {
        CompilationRejected(error.to_string()).into()
    } else {
        error.into()
    }
}

// Keep the recursive SV frontend out of the multi-backend dispatch frame.
#[cfg(feature = "systemverilog")]
fn build_sv(design: &Design, sources: &[(&str, &std::path::Path)]) -> Result<Box<dyn Backend>> {
    let emitted = super::veryl_sv::emit_veryl_sources(sources);
    Ok(Box::new(CeloxBackend(
        design
            .parameters
            .iter()
            .fold(
                Simulator::from_sv_sources(emitted.as_sv_sources(), &design.top)
                    .four_state(design.four_state)
                    .allow_inferred_latches(design.allow_inferred_latches),
                |builder, (name, value)| builder.param(name, *value),
            )
            .build()
            .map_err(classify_build_error)?,
    )))
}

pub fn run_case(name: &str, backend: &str) {
    let run = || {
        celox_test_suite::veryl::case(name)
            .unwrap_or_else(|| panic!("unknown Veryl test case: {name}"))
            .run(&mut |design| build(design, backend));
    };
    if backend == "sv" {
        // The recursive SV parser is close to libtest's default stack limit.
        // The shared driver adds call frames, so reserve room in this runner
        // instead of requiring RUST_MIN_STACK in every consuming environment.
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(8 * 1024 * 1024)
                .spawn_scoped(scope, run)
                .unwrap()
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        });
    } else {
        run();
    }
}
