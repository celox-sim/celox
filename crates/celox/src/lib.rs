mod backend;
mod debug;
mod diagnostics;
mod ir;
mod optimizer;
mod parser;
pub(crate) mod portable;
#[cfg(not(target_arch = "wasm32"))]
mod simulation;
mod simulator;
#[cfg(not(target_arch = "wasm32"))]
pub mod testbench;
mod testbench_compile;
pub(crate) mod timing;
#[cfg(not(target_arch = "wasm32"))]
mod vcd {
    pub use celox_runtime::{VcdSignalDesc, VcdWriter};
}
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
pub use backend::{EventHandle, LayoutRequirements, MemoryLayout, MemoryLayoutMode, get_byte_size};
#[cfg(not(target_arch = "wasm32"))]
pub use backend::{JitBackend, SimBackend};
pub use celox_design::{ElaboratedDesign, EventTopology, RuntimeSchema};
#[cfg(not(target_arch = "wasm32"))]
pub use celox_macros::veryl_test;
#[cfg(not(target_arch = "wasm32"))]
pub use debug::CompilationTraceResult;
pub use debug::{CompilationTrace, NativeProfileBlock, TraceOptions};
pub use diagnostics::{DiagnosticsOptions, RuntimeDiagnostics};
pub(crate) use fxhash::FxHashMap as HashMap;
pub(crate) use fxhash::FxHashSet as HashSet;
pub use ir::{
    AbsoluteAddr, AddrLookupError, InstancePath, LaidOutProgram, OptimizedSir, PortTypeKind,
    RuntimeErrorInfo, RuntimeProgram, SignalRef, SirProgram, UnoptimizedSir, VariableInfo,
    VerylFrontendLookup,
};
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
#[cfg(not(target_arch = "wasm32"))]
pub use backend::CraneliftDiagnostics;
#[cfg(not(target_arch = "wasm32"))]
pub use backend::CraneliftOptLevel;
#[cfg(not(target_arch = "wasm32"))]
pub use backend::CraneliftOptions;
#[cfg(not(target_arch = "wasm32"))]
pub use backend::RegallocAlgorithm;
#[cfg(target_arch = "x86_64")]
pub use backend::{NativeDiagnostics, NativeDumpOptions};
pub use celox_frontend_veryl::{LoweringPhase, ParserError};
pub use celox_slt::scheduler::SchedulerError;
pub use num_bigint::BigUint;
pub use optimizer::OptLevel;
pub use optimizer::OptimizeOptions;
pub use optimizer::SirDiagnostics;
pub use optimizer::SirPass;
#[cfg(not(target_arch = "wasm32"))]
pub use simulation::Simulation;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::DeadStorePolicy;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::RuntimeEvent;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::RuntimeEventDrain;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::RuntimeFormatContext;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::Simulator;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::SimulatorBuilder;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator::SimulatorOptions;
pub use simulator::render_diagnostic;
pub use simulator::{CodegenError, SimulatorError, SimulatorErrorKind};
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
