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
#[cfg(not(target_arch = "wasm32"))]
mod simulation;
mod simulator;
#[cfg(not(target_arch = "wasm32"))]
pub mod testbench;
pub(crate) mod timing;
#[cfg(not(target_arch = "wasm32"))]
mod vcd;
#[cfg(not(target_arch = "wasm32"))]
pub use vcd::{VcdSignalDesc, VcdWriter};

#[cfg(not(target_arch = "wasm32"))]
pub use backend::SimulatorErrorCode as RuntimeErrorCode;

#[cfg(not(target_arch = "wasm32"))]
pub struct IOContext<'a, B: backend::SimBackend = DefaultBackend> {
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
pub use backend::{EventHandle, MemoryLayout, get_byte_size};
#[cfg(not(target_arch = "wasm32"))]
pub use backend::{JitBackend, SimBackend};
pub use celox_macros::veryl_test;
#[cfg(not(target_arch = "wasm32"))]
pub use debug::CompilationTraceResult;
pub use debug::{CompilationTrace, TraceOptions};
pub(crate) use fxhash::FxHashMap as HashMap;
pub(crate) use fxhash::FxHashSet as HashSet;
pub use ir::{AbsoluteAddr, AddrLookupError, PortTypeKind, Program, SignalRef};
#[cfg(target_arch = "x86_64")]
pub mod native_backend {
    //! Re-exports for the native x86-64 backend (for testing/integration).
    pub use crate::backend::native::*;
}
#[cfg(target_arch = "x86_64")]
pub use backend::native::backend::NativeEventRef;
#[cfg(target_arch = "x86_64")]
pub use backend::native::{NativeBackend, SharedNativeCode};

/// Default simulation backend: NativeBackend on x86-64, JitBackend (Cranelift) elsewhere.
#[cfg(target_arch = "x86_64")]
pub type DefaultBackend = NativeBackend;
#[cfg(all(not(target_arch = "wasm32"), not(target_arch = "x86_64")))]
pub type DefaultBackend = backend::JitBackend;
pub use num_bigint::BigUint;
#[cfg(not(target_arch = "wasm32"))]
pub use optimizer::CraneliftOptLevel;
#[cfg(not(target_arch = "wasm32"))]
pub use optimizer::CraneliftOptions;
pub use optimizer::OptLevel;
pub use optimizer::OptimizeOptions;
#[cfg(not(target_arch = "wasm32"))]
pub use optimizer::RegallocAlgorithm;
pub use optimizer::SirPass;
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
#[cfg(not(target_arch = "wasm32"))]
pub use testbench::TestResult;
#[cfg(not(target_arch = "wasm32"))]
pub use testbench::{AssertionResult, SourceLocation, TestResultDetailed};
pub use veryl_metadata::{ClockType, ResetType};

// Re-exports needed for wasm32 builds
pub use backend::wasm_codegen;

// Public compilation API (available on all targets)
pub use simulator::compile_to_sir;

#[cfg(test)]
mod flatting_tests;
