mod backend;
mod context_width;
mod debug;
mod flatting;
mod ir;
mod logic_tree;
mod optimizer;
mod parser;
pub(crate) mod portable;
#[cfg(not(target_arch = "wasm32"))]
mod scheduler;
pub(crate) mod serde_helpers;
pub(crate) mod timing;
#[cfg(not(target_arch = "wasm32"))]
mod simulation;
mod simulator;
#[cfg(not(target_arch = "wasm32"))]
mod vcd;
#[cfg(not(target_arch = "wasm32"))]
pub use vcd::{VcdSignalDesc, VcdWriter};

#[cfg(not(target_arch = "wasm32"))]
pub use backend::SimulatorErrorCode as RuntimeErrorCode;

#[cfg(not(target_arch = "wasm32"))]
pub struct IOContext<'a, B: backend::SimBackend = backend::JitBackend> {
    pub(crate) backend: &'a mut B,
}

#[cfg(not(target_arch = "wasm32"))]
impl<'a, B: backend::SimBackend> IOContext<'a, B> {
    pub fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
        self.backend.set(signal, val);
    }
    pub fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
        self.backend.set_wide(signal, val);
    }
    pub fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint) {
        self.backend.set_four_state(signal, val, mask);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use backend::EventRef;
#[cfg(not(target_arch = "wasm32"))]
pub use backend::SharedJitCode;
#[cfg(not(target_arch = "wasm32"))]
pub use backend::wasm_runtime::WasmBackend;
#[cfg(not(target_arch = "wasm32"))]
pub use backend::{JitBackend, SimBackend};
pub use backend::{EventHandle, MemoryLayout, get_byte_size};
pub use celox_macros::veryl_test;
#[cfg(not(target_arch = "wasm32"))]
pub use debug::CompilationTraceResult;
pub use debug::{CompilationTrace, TraceOptions};
pub(crate) use fxhash::FxHashMap as HashMap;
pub(crate) use fxhash::FxHashSet as HashSet;
pub use ir::{AbsoluteAddr, AddrLookupError, PortTypeKind, SignalRef};
pub use num_bigint::BigUint;
#[cfg(not(target_arch = "wasm32"))]
pub use optimizer::CraneliftOptLevel;
#[cfg(not(target_arch = "wasm32"))]
pub use optimizer::CraneliftOptions;
pub use optimizer::OptimizeOptions;
#[cfg(not(target_arch = "wasm32"))]
pub use optimizer::RegallocAlgorithm;
pub use parser::LoweringPhase;
pub use parser::ParserError;
pub use parser::SchedulerError;
#[cfg(not(target_arch = "wasm32"))]
pub use simulation::Simulation;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::DeadStorePolicy;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::Simulator;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::SimulatorBuilder;
pub use simulator::SimulatorError;
pub use simulator::SimulatorErrorKind;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::SimulatorOptions;
pub use simulator::render_diagnostic;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::{InstanceHierarchy, NamedEvent, NamedSignal};
pub use veryl_metadata::{ClockType, ResetType};

// Re-exports needed for wasm32 builds
pub use backend::wasm_codegen;
pub use ir::Program;

// Public compilation API (available on all targets)
pub use simulator::compile_to_sir;

#[cfg(test)]
mod flatting_tests;
