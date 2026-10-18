//! Runs SystemVerilog suite cases on the Celox backends.

use celox::{BigUint, RuntimeEvent, SimBackend, Simulation, Simulator};
use celox_test_suite::{Backend, CompilationRejected, Design, Result, SignalPath};

struct CeloxBackend<B: SimBackend>(Simulator<B>);

/// A timed case: the design's own processes and clocks run in a
/// [`Simulation`]. A host write is settled at the current time by the next
/// run operation.
struct TimedBackend<B: SimBackend> {
    sim: Simulation<B>,
    /// Runtime events not yet taken as output.
    events: Vec<RuntimeEvent>,
    finished: bool,
}

impl<B: SimBackend> TimedBackend<B> {
    fn new(sim: Simulation<B>) -> Self {
        Self {
            sim,
            events: Vec::new(),
            finished: false,
        }
    }

    fn signal(&self, path: &SignalPath) -> celox::SignalRef {
        let instances: Vec<_> = path
            .instances
            .iter()
            .map(|i| (i.name.as_str(), i.index.unwrap_or(0)))
            .collect();
        self.sim.child_signal(&instances, &path.name)
    }

    /// Collect the design's runtime events; a failed assertion fails the
    /// case, and a `$finish` outside the run operations is an error.
    fn capture_events(&mut self) -> Result<()> {
        let events = self.sim.drain_runtime_events();
        let error = events.iter().find_map(|event| match event {
            RuntimeEvent::AssertFatal { message } => Some(format!(
                "fatal assertion: {}",
                String::from_utf8_lossy(message)
            )),
            RuntimeEvent::AssertContinue { message } => Some(format!(
                "failed assertion: {}",
                String::from_utf8_lossy(message)
            )),
            RuntimeEvent::Missed { count } => Some(format!("{count} runtime events were missed")),
            _ => None,
        });
        self.finished |= self.sim.is_finished()
            || events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::Finish));
        self.events.extend(events);
        match error {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }

    /// Capture events after an operation that does not advance time.
    fn checked(&mut self, operation: &str) -> Result<()> {
        let finished = self.finished;
        self.capture_events()?;
        if !finished && self.finished {
            return Err(format!("the design finished during {operation}").into());
        }
        Ok(())
    }
}

fn collect_output(events: Vec<RuntimeEvent>) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    for event in events {
        match event {
            RuntimeEvent::Display { message } => {
                output.extend_from_slice(&message);
                output.push(b'\n');
            }
            RuntimeEvent::Write { message } => output.extend_from_slice(&message),
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
        let signal = self.signal(path);
        self.sim.modify(|io| {
            if mask == BigUint::default() {
                io.set_wide(signal, payload);
            } else {
                io.set_four_state(signal, payload, mask);
            }
        })?;
        self.checked("a write")
    }

    fn read(&mut self, path: &SignalPath) -> Result<(BigUint, BigUint)> {
        self.checked("a read")?;
        let signal = self.signal(path);
        Ok(self.sim.get_four_state(signal))
    }

    fn eval_comb(&mut self) -> Result<()> {
        self.sim.eval_comb()?;
        self.checked("eval")
    }

    fn tick(&mut self, _event: &str) -> Result<()> {
        Err("tick is not available in a timed case; the design drives its clocks".into())
    }

    fn take_output(&mut self) -> Result<Vec<u8>> {
        self.checked("output collection")?;
        collect_output(std::mem::take(&mut self.events))
    }

    fn run_until(&mut self, time: u64) -> Result<()> {
        let now = self.sim.time();
        if time < now {
            return Err(format!("run_until {time}: the simulation is already at {now}").into());
        }
        self.sim.run_until(time)?;
        self.capture_events()?;
        if self.finished {
            return Err(format!(
                "run_until {time}: the design finished the simulation at {}",
                self.sim.time()
            )
            .into());
        }
        Ok(())
    }

    fn run_to_finish(&mut self) -> Result<()> {
        if self.finished {
            return Err("run_to_finish: the design had already finished".into());
        }
        while !self.finished {
            let step = self.sim.step()?;
            self.capture_events()?;
            if step.is_none() {
                break;
            }
        }
        if !self.finished {
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
        // The suite pulses the port as the external testbenches do: to its
        // inactive level, to its active level, and back unless it was active.
        let handle = self.0.event(event);
        let signal = self.0.signal(event);
        let active = self.0.event_edges()[celox::EventHandle::id(&handle)].active_edge();
        let (value, mask) = self.0.get_four_state(signal);
        let zero = celox::BigUint::default();
        let was_active = mask == zero && (value != zero) == (active == celox::Edge::Rising);
        self.0.inactive_edge(handle)?;
        self.0.active_edge(handle)?;
        if !was_active {
            self.0.inactive_edge(handle)?;
        }
        Ok(())
    }

    fn take_output(&mut self) -> Result<Vec<u8>> {
        collect_output(self.0.drain_runtime_events())
    }
}

fn build(design: &Design, backend: &str) -> Result<Box<dyn Backend>> {
    celox_test_suite::validate_timed_design(design)?;
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
            "native" => Box::new(TimedBackend::new(builder.build_native()?)),
            "cranelift" => Box::new(TimedBackend::new(builder.build_cranelift()?)),
            "native-parallel" => Box::new(TimedBackend::new(
                builder
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_native()?,
            )),
            "cranelift-parallel" => Box::new(TimedBackend::new(
                builder
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_cranelift()?,
            )),
            "wasm" => Box::new(TimedBackend::new(builder.build_wasm()?)),
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
