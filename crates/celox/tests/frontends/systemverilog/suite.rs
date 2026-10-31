//! Runs SystemVerilog suite cases on the Celox backends.

use celox::{BigUint, RuntimeEvent, SchedulerMode, SimBackend, Simulation, Simulator};
use celox_test_suite::{Backend, CompilationRejected, Design, Result, SignalPath};

struct CeloxBackend<B: SimBackend>(Simulator<B>);

/// Simulation time units in one time unit of a timed case. A case names
/// times in the design's time unit, which is the default 1 ns because timed
/// cases declare no scale, and a `Simulation` counts the global precision,
/// the default 1 ps.
const UNIT: u64 = 1000;

/// How far past the next event each run of `run_to_finish` goes, in
/// simulation time units.
const FINISH_STRETCH: u64 = 1 << 20;

/// A timed case: the design's own processes and clocks run in a
/// [`Simulation`]. A host write is settled at the current time by the next
/// run operation, read, eval or output collection.
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
            RuntimeEvent::Report(report) if report.severity >= celox::Severity::Error => {
                Some(format!(
                    "{:?}: {}",
                    report.severity,
                    String::from_utf8_lossy(&report.message)
                ))
            }
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

    /// Settle host writes and the work they wake at the current time, as
    /// the reference simulators do before a read, then capture events.
    fn settled(&mut self, operation: &str) -> Result<()> {
        let finished = self.finished;
        if !finished {
            self.sim.run_until(self.sim.time())?;
        }
        self.capture_events()?;
        if !finished && self.finished {
            return Err(format!("the design finished during {operation}").into());
        }
        Ok(())
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

/// In differential mode, a case either checked its steps against the shadow
/// or reports why none was checked; it never passes unchecked silently.
impl<B: SimBackend> Drop for TimedBackend<B> {
    #[allow(clippy::disallowed_macros)] // The skip report goes to the test log.
    fn drop(&mut self) {
        if std::thread::panicking()
            || !matches!(
                scheduler_mode(),
                SchedulerMode::Differential | SchedulerMode::DifferentialRuns
            )
        {
            return;
        }
        let stats = self.sim.scheduler_stats();
        match stats.unchecked {
            Some(reason) => eprintln!("differential: unchecked: {reason}"),
            // A design without processes, waits or scheduled events may
            // settle host writes on read and only move time.
            None if stats.checked_steps == 0 => {
                eprintln!("differential: unchecked: the case took no scheduler step")
            }
            None => {}
        }
        eprintln!(
            "differential: compiled slots: {}",
            u8::from(stats.compiled_slots > 0)
        );
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
        self.settled("a read")?;
        let signal = self.signal(path);
        Ok(self.sim.get_four_state(signal))
    }

    fn eval_comb(&mut self) -> Result<()> {
        self.sim.eval_comb()?;
        self.settled("eval")
    }

    fn tick(&mut self, _event: &str) -> Result<()> {
        Err("tick is not available in a timed case; the design drives its clocks".into())
    }

    fn take_output(&mut self) -> Result<Vec<u8>> {
        self.settled("output collection")?;
        collect_output(std::mem::take(&mut self.events))
    }

    fn run_until(&mut self, time: u64) -> Result<()> {
        let now = self.sim.time() / UNIT;
        if time < now {
            return Err(format!("run_until {time}: the simulation is already at {now}").into());
        }
        self.sim.run_until(time * UNIT)?;
        self.capture_events()?;
        if self.finished {
            return Err(format!(
                "run_until {time}: the design finished the simulation at {}",
                self.sim.time() / UNIT
            )
            .into());
        }
        Ok(())
    }

    fn run_to_finish(&mut self) -> Result<()> {
        if self.finished {
            return Err("run_to_finish: the design had already finished".into());
        }
        // Runs to the next events and a stretch beyond, as a host loop
        // would, so that a compiled scheduler may run up to the finish.
        while !self.finished {
            let Some(next) = self.sim.next_event_time() else {
                break;
            };
            self.sim.run_until(next.saturating_add(FINISH_STRETCH))?;
            self.capture_events()?;
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

/// The scheduler mode of timed cases: `CELOX_SCHEDULER` set to
/// `reference`, `compiled`, `differential` or `differential-runs`, by
/// default the library's.
#[allow(clippy::disallowed_methods)] // Opt-in test configuration is environment-driven.
fn scheduler_mode() -> SchedulerMode {
    match std::env::var("CELOX_SCHEDULER").as_deref() {
        Ok("reference") => SchedulerMode::Reference,
        Ok("compiled") => SchedulerMode::Compiled,
        Ok("differential") => SchedulerMode::Differential,
        Ok("differential-runs") => SchedulerMode::DifferentialRuns,
        Ok(other) => panic!("unknown CELOX_SCHEDULER mode `{other}`"),
        Err(_) => SchedulerMode::default(),
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
            Simulation::from_sv_sources(sources, &design.top)
                .scheduler_mode(scheduler_mode())
                .four_state(design.four_state)
                .allow_inferred_latches(design.allow_inferred_latches),
            |builder, (name, value)| builder.param(name, *value),
        );
        return Ok(match backend {
            "interpreter" => Box::new(TimedBackend::new(builder.build_interpreter()?)),
            "native" => Box::new(TimedBackend::new(builder.build_native()?)),
            // The image goes through its container bytes, as an attached
            // image does, and the simulation takes its metadata from it.
            "native-image" => {
                let image = builder.compile_native()?.into_program_image();
                let image =
                    celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes()?)?;
                Box::new(TimedBackend::new(
                    Simulation::from_sv_sources(Vec::new(), &design.top)
                        .scheduler_mode(scheduler_mode())
                        .build_native_from_image(image)?,
                ))
            }
            #[cfg(feature = "cranelift")]
            "cranelift" => Box::new(TimedBackend::new(builder.build_cranelift()?)),
            "native-parallel" => Box::new(TimedBackend::new(
                builder
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_native()?,
            )),
            #[cfg(feature = "cranelift")]
            "cranelift-parallel" => Box::new(TimedBackend::new(
                builder
                    .threads(4)
                    .parallel_partition(celox::ParallelPartition::Always)
                    .build_cranelift()?,
            )),
            #[cfg(feature = "wasm-runtime")]
            "wasm" => Box::new(TimedBackend::new(builder.build_wasm()?)),
            _ => return Err(format!("unknown backend: {backend}").into()),
        });
    }
    let builder = design.parameters.iter().fold(
        Simulator::from_sv_sources(sources, &design.top)
            .four_state(design.four_state)
            .allow_inferred_latches(design.allow_inferred_latches),
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
        "native-image" => {
            let image = builder
                .compile_native()
                .map_err(rejected)?
                .into_program_image();
            let image =
                celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes()?)?;
            Box::new(CeloxBackend(
                Simulator::from_sv_sources(Vec::new(), &design.top)
                    .build_native_from_image(image)
                    .map_err(rejected)?,
            ))
        }
        #[cfg(feature = "cranelift")]
        "cranelift" => Box::new(CeloxBackend(builder.build_cranelift().map_err(rejected)?)),
        "native-parallel" => Box::new(CeloxBackend(
            builder
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
                .build_native()
                .map_err(rejected)?,
        )),
        #[cfg(feature = "cranelift")]
        "cranelift-parallel" => Box::new(CeloxBackend(
            builder
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
                .build_cranelift()
                .map_err(rejected)?,
        )),
        #[cfg(feature = "wasm-runtime")]
        "wasm" => Box::new(CeloxBackend(builder.build_wasm().map_err(rejected)?)),
        _ => return Err(format!("unknown backend: {backend}").into()),
    })
}

/// Run the suite case `name` on `backend`. The SV frontend is recursive, so
/// the case runs on a thread with room on its stack.
pub fn run_case(name: &str, backend: &str) {
    if super::vtest_exec::dispatch("sv", name, backend) {
        return;
    }
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

/// Timed cases whose every kernel the compiled scheduler must run: clocks
/// generated by delay processes, with the domains, resets and finishes
/// around them.
const COMPILED_CASES: [&str; 6] = [
    "timing::always_with_a_delay_generates_a_clock",
    "timing::asynchronous_reset_driven_by_a_delay_process",
    "timing::generated_clocks_with_a_common_edge_sample_each_other",
    "timing::script_writes_between_runs_reach_generated_clock_domains",
    "timing::finish_from_a_delay_process_between_clock_edges",
    "timing::finish_in_an_always_process_ends_the_simulation",
];

/// The share of the kernels of the timed cases the compiled scheduler must
/// run at least.
const COMPILED_FLOOR: f64 = 0.3;

/// How the compiled scheduler runs the kernels of every timed suite case:
/// how many it compiles and why it leaves the others to the reference loop.
/// The report goes to the test output; the cases of [`COMPILED_CASES`] must
/// compile entirely, and the share of compiled kernels must stay above
/// [`COMPILED_FLOOR`].
#[test]
#[allow(clippy::disallowed_macros)] // The report goes to the test log.
fn compiled_scheduler_coverage() {
    use std::fmt::Write;
    let (report, compiled, total, incomplete) = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn_scoped(scope, || {
                let mut report = String::new();
                let (mut compiled_total, mut total) = (0, 0);
                let mut incomplete = Vec::new();
                for case in celox_test_suite::sv::cases() {
                    let design = case.design();
                    if !design.timed {
                        continue;
                    }
                    let sources = design
                        .sources
                        .iter()
                        .map(|source| (source.text.as_str(), source.path.as_path()))
                        .collect::<Vec<_>>();
                    let builder = design.parameters.iter().fold(
                        Simulation::from_sv_sources(sources, &design.top)
                            .scheduler_mode(SchedulerMode::Compiled)
                            .four_state(design.four_state)
                            .allow_inferred_latches(design.allow_inferred_latches),
                        |builder, (name, value)| builder.param(name, *value),
                    );
                    let Ok(sim) = builder.build_interpreter() else {
                        continue;
                    };
                    let kernels = sim.scheduler_stats().kernels;
                    let compiled = kernels
                        .iter()
                        .filter(|kernel| matches!(kernel, celox::KernelSchedule::Compiled))
                        .count();
                    compiled_total += compiled;
                    total += kernels.len();
                    if COMPILED_CASES.contains(&case.name) && compiled != kernels.len() {
                        incomplete.push(case.name);
                    }
                    let mut reasons = std::collections::BTreeMap::<&str, usize>::new();
                    for kernel in &kernels {
                        if let celox::KernelSchedule::Reference(reason) = kernel {
                            *reasons.entry(reason).or_default() += 1;
                        }
                    }
                    write!(report, "{}: {compiled}/{}", case.name, kernels.len()).unwrap();
                    for (reason, count) in reasons {
                        write!(report, "; {reason} ({count})").unwrap();
                    }
                    report.push('\n');
                }
                (report, compiled_total, total, incomplete)
            })
            .unwrap()
            .join()
            .unwrap()
    });
    eprintln!("compiled scheduler coverage: {compiled}/{total} kernels\n{report}");
    assert!(incomplete.is_empty(), "kernels fall back in {incomplete:?}");
    assert!(
        compiled as f64 >= COMPILED_FLOOR * total as f64,
        "only {compiled} of {total} kernels compile"
    );
}
