//! Celox and reference adapters for the independent Veryl corpus.
#![allow(dead_code)]

use celox::{BigUint, SimBackend, Simulator};
use veryl_test_suite::{Backend, Design, Result, SignalPath};

struct CeloxBackend<B: SimBackend>(Simulator<B>);

impl<B: SimBackend> CeloxBackend<B> {
    fn signal(&self, path: &SignalPath) -> celox::SignalRef {
        let instances: Vec<_> = path
            .instances
            .iter()
            .map(|i| (i.name.as_str(), i.index))
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

struct VerylBackend(super::veryl_sim::VerylSimAdapter);

impl VerylBackend {
    fn signal(&mut self, path: &SignalPath) -> super::veryl_sim::VerylSignalRef {
        let instances: Vec<_> = path
            .instances
            .iter()
            .map(|i| (i.name.as_str(), i.index))
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

fn build(design: &Design, backend: &str) -> Result<Box<dyn Backend>> {
    let sources = design
        .sources
        .iter()
        .map(|source| (source.text.as_str(), source.path.as_path()))
        .collect::<Vec<_>>();
    if backend == "veryl" {
        return Ok(Box::new(VerylBackend(
            super::veryl_sim::build_veryl_adapter(&sources, &design.top, design.four_state),
        )));
    }
    #[cfg(feature = "systemverilog")]
    if backend == "sv" {
        return build_sv(design, &sources);
    }
    let builder = Simulator::from_sources(sources, &design.top)
        .four_state(design.four_state)
        .allow_always_ff_function_effects(true);
    Ok(match backend {
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        "native" => Box::new(CeloxBackend(builder.build_native()?)),
        "cranelift" => Box::new(CeloxBackend(builder.build_cranelift()?)),
        "wasm" => Box::new(CeloxBackend(builder.build_wasm()?)),
        "interp" => Box::new(CeloxBackend(builder.build_interpreter()?)),
        _ => return Err(format!("unknown suite backend: {backend}").into()),
    })
}

// Keep the recursive SV frontend out of the multi-backend dispatch frame.
#[cfg(feature = "systemverilog")]
fn build_sv(design: &Design, sources: &[(&str, &std::path::Path)]) -> Result<Box<dyn Backend>> {
    let emitted = super::veryl_sv::emit_veryl_sources(sources);
    Ok(Box::new(CeloxBackend(
        Simulator::from_sv_sources(emitted.as_sv_sources(), &design.top)
            .four_state(design.four_state)
            .build()?,
    )))
}

pub fn run_case(name: &str, backend: &str) {
    let run = || {
        veryl_test_suite::case(name)
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
