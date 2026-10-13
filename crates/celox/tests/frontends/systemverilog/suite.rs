//! Runs SystemVerilog suite cases on the Celox backends.

use celox::{BigUint, RuntimeEvent, SimBackend, Simulation, Simulator};
use celox_test_suite::{Backend, CompilationRejected, Design, Result, SignalPath};

struct CeloxBackend<B: SimBackend>(Simulator<B>);

/// A timed case: the design's own processes and clocks run.
struct TimedBackend<B: SimBackend>(Simulation<B>, Vec<RuntimeEvent>, bool);
impl<B: SimBackend> TimedBackend<B> {
    fn new(sim: Simulation<B>) -> Self {
        Self(sim, Vec::new(), false)
    }
    fn capture_events(&mut self) -> Result<()> {
        let events = self.0.drain_runtime_events();
        self.2 |= events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Finish));
        let error = events.iter().find_map(|event| match event {
            RuntimeEvent::AssertFatal { message } => Some(format!(
                "fatal assertion: {}",
                String::from_utf8(message.clone()).unwrap_or_else(|_| format!("{message:?}"))
            )),
            RuntimeEvent::Missed { count } => Some(format!("{count} runtime events were missed")),
            _ => None,
        });
        self.1.extend(events);
        if let Some(error) = error {
            Err(error.into())
        } else {
            Ok(())
        }
    }
    fn settle_checked(&mut self) -> Result<()> {
        let already_finished = self.finished();
        self.0.settle()?;
        self.capture_events()?;
        if !already_finished && self.finished() {
            return Err("the design finished outside run_to_finish during settling".into());
        }
        Ok(())
    }
    fn finished(&self) -> bool {
        self.0.is_finished() || self.2
    }
}

fn signal_of<B: SimBackend>(sim: &Simulation<B>, path: &SignalPath) -> celox::SignalRef {
    let instances: Vec<_> = path
        .instances
        .iter()
        .map(|i| (i.name.as_str(), i.index.unwrap_or(0)))
        .collect();
    sim.child_signal(&instances, &path.name)
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
        let signal = signal_of(&self.0, path);
        if mask == BigUint::default() {
            self.0.set_wide(signal, payload);
        } else {
            self.0.set_four_state(signal, payload, mask);
        }
        Ok(())
    }

    fn read(&mut self, path: &SignalPath) -> Result<(BigUint, BigUint)> {
        self.settle_checked()?;
        let signal = signal_of(&self.0, path);
        Ok(self.0.get_four_state(signal))
    }

    /// Settling also resumes the processes the writes woke, as the external
    /// simulators' settle step does.
    fn eval_comb(&mut self) -> Result<()> {
        self.settle_checked()?;
        Ok(())
    }

    fn tick(&mut self, _event: &str) -> Result<()> {
        Err("tick is not available in a timed case; the design drives its clocks".into())
    }

    fn take_output(&mut self) -> Result<Vec<u8>> {
        self.settle_checked()?;
        collect_output(std::mem::take(&mut self.1))
    }

    fn run_until(&mut self, time: u64) -> Result<()> {
        let now = self.0.time();
        if time < now {
            return Err(format!("run_until {time}: the simulation is already at {now}").into());
        }
        self.0.settle()?;
        self.0.run_until(time)?;
        self.capture_events()?;
        if self.finished() {
            return Err(format!(
                "run_until {time}: the design finished the simulation at {}",
                self.0.time()
            )
            .into());
        }
        Ok(())
    }

    fn run_to_finish(&mut self) -> Result<()> {
        self.capture_events()?;
        if self.finished() {
            return Err(
                "run_to_finish: the design already finished before the operation began".into(),
            );
        }
        while !self.finished() {
            let step = self.0.step()?;
            self.capture_events()?;
            if step.is_none() {
                break;
            }
        }
        if !self.finished() {
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

#[test]
fn run_to_finish_reaches_the_last_scheduler_timestamp() {
    let source = "module Top(output bit done); initial begin #(64'hffffffffffffffff); done = 1; $finish; end endmodule";
    let simulation =
        celox::Simulation::from_sv_sources(vec![(source, std::path::Path::new("last.sv"))], "Top")
            .build()
            .unwrap();
    let mut backend = TimedBackend::new(simulation);
    backend.run_to_finish().unwrap();
    assert_eq!(backend.0.time(), u64::MAX);
    assert_eq!(backend.0.get(backend.0.signal("done")), 1u8.into());
}

#[test]
fn run_to_finish_rejects_a_finish_during_an_earlier_settle() {
    for operation in 0..3 {
        let source = "module Top(input bit go, output bit done); initial begin wait(go); done=1; $finish; end endmodule";
        let simulation =
            Simulation::from_sv_sources(vec![(source, std::path::Path::new("early.sv"))], "Top")
                .build()
                .unwrap();
        let mut backend = TimedBackend::new(simulation);
        backend.run_until(0).unwrap();
        let go = backend.0.signal("go");
        backend.0.set_wide(go, 1u8.into());
        match operation {
            0 => assert!(
                backend
                    .eval_comb()
                    .unwrap_err()
                    .to_string()
                    .contains("finished outside")
            ),
            1 => {
                backend
                    .read(&SignalPath {
                        instances: Vec::new(),
                        name: "done".into(),
                    })
                    .unwrap_err();
            }
            _ => {
                assert!(
                    backend
                        .take_output()
                        .unwrap_err()
                        .to_string()
                        .contains("finished outside")
                );
            }
        }
        assert!(
            backend
                .run_to_finish()
                .unwrap_err()
                .to_string()
                .contains("already finished")
        );
    }
}

#[test]
fn run_until_settles_a_host_edge_without_a_runtime_process() {
    let source = "module Top(input bit clk);always_ff @(posedge clk)$finish;endmodule";
    for name in [
        "native",
        "cranelift",
        "native-parallel",
        "cranelift-parallel",
        "wasm",
    ] {
        let design = celox_test_suite::load_group(
            "finish-edge.vtest",
            &format!(
                "(group protocol (category sequential))(case finish (source \"finish-edge.sv\" #\"{source}\"#)(top Top)(set clk 1)(run_until 0))"
            ),
            celox_test_suite::no_std_library,
        )[0];
        let mut backend = build(&design.design(), name).unwrap();
        backend
            .write(
                &SignalPath {
                    instances: Vec::new(),
                    name: "clk".into(),
                },
                1u8.into(),
                0u8.into(),
            )
            .unwrap();
        assert!(
            backend
                .run_until(0)
                .unwrap_err()
                .to_string()
                .contains("finished"),
            "{name}"
        );
    }
}

#[test]
fn buffered_host_fatal_cannot_be_hidden_by_a_later_finish() {
    let source = "module Top(input bit go);always_comb if(go)$fatal(1,\"host fatal\");initial #1 $finish;endmodule";
    let text = format!(
        "(group protocol (category sequential))(case fatal (source \"fatal.sv\" #\"{source}\"#)(top Top)(set go 1)(run_to_finish))"
    );
    let case =
        celox_test_suite::load_group("fatal.vtest", &text, celox_test_suite::no_std_library)[0];
    for name in [
        "native",
        "cranelift",
        "native-parallel",
        "cranelift-parallel",
        "wasm",
    ] {
        let mut backend = build(&case.design(), name).unwrap();
        backend
            .write(
                &SignalPath {
                    instances: Vec::new(),
                    name: "go".into(),
                },
                1u8.into(),
                0u8.into(),
            )
            .unwrap();
        assert!(
            backend
                .run_to_finish()
                .unwrap_err()
                .to_string()
                .contains("fatal"),
            "{name}"
        );
    }
}
