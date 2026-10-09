//! Runs SystemVerilog suite cases on the Celox backends.

use celox::{BigUint, SimBackend, Simulator};
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

    fn tick(&mut self, event: &str) -> Result<()> {
        self.0.tick(self.0.event(event)).map_err(Into::into)
    }
}

fn build(design: &Design, backend: &str) -> Result<Box<dyn Backend>> {
    let sources = design
        .sources
        .iter()
        .map(|source| (source.text.as_str(), source.path.as_path()))
        .collect::<Vec<_>>();
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
