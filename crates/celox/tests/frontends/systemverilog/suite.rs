//! Runs SystemVerilog suite cases on the Celox backends.

use celox::{BigUint, RuntimeEvent, SimBackend, Simulation, Simulator};
use celox_test_suite::{Backend, CompilationRejected, Design, Result, SignalPath};

struct CeloxBackend<B: SimBackend>(Simulator<B>);

/// A timed case: the design's own processes and clocks run.
struct TimedBackend<B: SimBackend>(Simulation<B>);

fn signal_of<B: SimBackend>(sim: &Simulation<B>, path: &SignalPath) -> celox::SignalRef {
    let instances: Vec<_> = path
        .instances
        .iter()
        .map(|i| (i.name.as_str(), i.index.unwrap_or(0)))
        .collect();
    sim.child_signal(&instances, &path.name)
}

fn collect_output(events: Vec<RuntimeEvent>) -> Result<String> {
    let mut output = String::new();
    for event in events {
        match event {
            RuntimeEvent::Display { message } => {
                output.push_str(&message);
                output.push('\n');
            }
            RuntimeEvent::Write { message } => output.push_str(&message),
            RuntimeEvent::Missed { count } => {
                return Err(format!("{count} runtime events were missed").into());
            }
            // Simulators print severity messages in their own words.
            _ => {}
        }
    }
    Ok(output)
}

impl<B: SimBackend> Backend for TimedBackend<B> {
    fn write(&mut self, path: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        let signal = signal_of(&self.0, path);
        if mask == BigUint::default() {
            self.0.set_wide(signal, payload);
        } else {
            self.0.set_four_state(signal, payload, mask);
        }
        Ok(())
    }

    fn read(&mut self, path: &SignalPath) -> Result<(BigUint, BigUint)> {
        let signal = signal_of(&self.0, path);
        Ok(self.0.get_four_state(signal))
    }

    /// Settling also resumes the processes the writes woke, as the external
    /// simulators' settle step does.
    fn eval_comb(&mut self) -> Result<()> {
        self.0.settle().map_err(Into::into)
    }

    fn tick(&mut self, _event: &str) -> Result<()> {
        Err("tick is not available in a timed case; the design drives its clocks".into())
    }

    fn take_output(&mut self) -> Result<String> {
        collect_output(self.0.drain_runtime_events())
    }

    fn run_until(&mut self, time: u64) -> Result<()> {
        let now = self.0.time();
        if time < now {
            return Err(format!("run_until {time}: the simulation is already at {now}").into());
        }
        self.0.run_until(time)?;
        if self.0.is_finished() {
            return Err(format!(
                "run_until {time}: the design finished the simulation at {}",
                self.0.time()
            )
            .into());
        }
        Ok(())
    }

    fn run_to_finish(&mut self) -> Result<()> {
        self.0.run_until(u64::MAX - 1)?;
        if !self.0.is_finished() {
            return Err(
                "run_to_finish: the simulation ran out of events before a process finished it"
                    .into(),
            );
        }
        Ok(())
    }
}

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

    fn tick(&mut self, event: &str) -> Result<()> {
        self.0.tick(self.0.event(event)).map_err(Into::into)
    }

    fn take_output(&mut self) -> Result<String> {
        collect_output(self.0.drain_runtime_events())
    }
}

fn build(design: &Design, backend: &str) -> Result<Box<dyn Backend>> {
    let sources = design
        .sources
        .iter()
        .map(|source| (source.text.as_str(), source.path.as_path()))
        .collect::<Vec<_>>();
    if design.timed {
        let builder = design.parameters.iter().fold(
            Simulation::from_sv_sources(sources, &design.top).four_state(design.four_state),
            |builder, (name, value)| builder.param(name, *value),
        );
        return Ok(match backend {
            "native" => Box::new(TimedBackend(builder.build_native()?)),
            "cranelift" => Box::new(TimedBackend(builder.build_cranelift()?)),
            "native-parallel" => Box::new(TimedBackend(
                builder
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_native()?,
            )),
            "cranelift-parallel" => Box::new(TimedBackend(
                builder
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_cranelift()?,
            )),
            "wasm" => Box::new(TimedBackend(builder.build_wasm()?)),
            _ => return Err(format!("unknown backend: {backend}").into()),
        });
    }
    let builder = design.parameters.iter().fold(
        Simulator::from_sv_sources(sources, &design.top).four_state(design.four_state),
        |builder, (name, value)| builder.param(name, *value),
    );
    // Only language errors satisfy a rejection case; an unsupported construct
    // or a code generation failure fails it.
    let rejected = |error: celox::SimulatorError| -> celox_test_suite::Error {
        let language = matches!(
            error.kind(),
            celox::SimulatorErrorKind::Analyzer(_)
                | celox::SimulatorErrorKind::Frontend(_)
                | celox::SimulatorErrorKind::SIRParser(celox::ParserError::IllegalContext { .. })
        );
        if language {
            CompilationRejected(error.to_string()).into()
        } else {
            error.into()
        }
    };
    Ok(match backend {
        "native" => Box::new(CeloxBackend(builder.build_native().map_err(rejected)?)),
        "cranelift" => Box::new(CeloxBackend(builder.build_cranelift().map_err(rejected)?)),
        "native-parallel" => Box::new(CeloxBackend(
            builder
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
                .build_native()
                .map_err(rejected)?,
        )),
        "cranelift-parallel" => Box::new(CeloxBackend(
            builder
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
                .build_cranelift()
                .map_err(rejected)?,
        )),
        "wasm" => Box::new(CeloxBackend(builder.build_wasm().map_err(rejected)?)),
        _ => return Err(format!("unknown backend: {backend}").into()),
    })
}

/// Run the suite case `name` on `backend`. The SV frontend is recursive, so
/// the case runs on a thread with room on its stack.
pub fn run_case(name: &'static str, backend: &'static str) {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(scope, || {
                celox_test_suite::sv::case(name)
                    .unwrap_or_else(|| panic!("unknown SystemVerilog test case: {name}"))
                    .run(&mut |design| build(design, backend));
            })
            .unwrap()
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
}
