mod backend;
#[cfg(feature = "host-runtime")]
mod component;
#[cfg(feature = "host-runtime")]
pub use component::{register_static_component, register_static_component_manifest};
mod debug;
mod diagnostics;
mod ir;
mod optimizer;
mod parser;
pub(crate) mod portable;
#[cfg(feature = "host-runtime")]
mod simulation;
mod simulator;
#[cfg(feature = "host-runtime")]
pub mod testbench;
mod testbench_compile;
pub(crate) mod timing;
pub use backend::SimulatorErrorCode as RuntimeErrorCode;
pub use backend::{
    EventHandle, LayoutRequirements, MemoryLayout, MemoryLayoutMode, SimBackend, get_byte_size,
};
pub use celox_design::{DomainKind, ElaboratedDesign, EventTopology, RuntimeSchema};
pub use celox_frontend_veryl::{FrontendDiagnostic, LoweringPhase, ParserError};
pub use celox_runtime::{
    DesignReflection, ReflectionScope, ReflectionScopeId, ReflectionSignal, ReflectionSignalId,
    SignalDirection,
};
pub use celox_slt::scheduler::SchedulerError;
pub use debug::{CompilationTrace, NativeProfileBlock, TraceOptions};
pub use diagnostics::RuntimeDiagnostics;
pub(crate) use fxhash::FxHashMap as HashMap;
pub(crate) use fxhash::FxHashSet as HashSet;
pub use ir::{
    AbsoluteAddr, AddrLookupError, InstancePath, LaidOutProgram, OptimizedSir, PortTypeKind,
    RuntimeErrorInfo, RuntimeProgram, SignalRef, SirProgram, UnoptimizedSir, VariableInfo,
    VerylFrontendLookup,
};
pub use num_bigint::BigUint;
pub use optimizer::OptLevel;
pub use optimizer::OptimizeOptions;
pub use optimizer::SirDiagnostics;
pub use optimizer::SirPass;
pub use simulator::render_diagnostic;
pub use simulator::{CodegenError, CompilationWarning, SimulatorError, SimulatorErrorKind};
pub use veryl_metadata::{ClockType, ResetType};

#[cfg(feature = "host-runtime")]
mod host_api {
    use crate::SimBackend;
    pub use crate::backend::wasm_runtime::WasmBackend;
    pub use crate::backend::{
        CraneliftDiagnostics, CraneliftOptLevel, CraneliftOptions, EventRef, JitBackend,
        RegallocAlgorithm, SharedJitCode,
    };
    pub use crate::debug::CompilationTraceResult;
    pub use crate::diagnostics::DiagnosticsOptions;
    pub use crate::simulation::Simulation;
    pub use crate::simulator::{
        DeadStorePolicy, InstanceHierarchy, NamedEvent, NamedSignal, RuntimeEvent,
        RuntimeEventDrain, RuntimeFormatContext, Simulator, SimulatorBuilder, SimulatorOptions,
    };
    pub use crate::testbench::{AssertionResult, SourceLocation, TestResult, TestResultDetailed};
    pub use celox_macros::veryl_test;
    pub use celox_runtime::{VcdSignalDesc, VcdWriter};

    pub struct IOContext<'a, B: SimBackend = DefaultBackend> {
        pub(crate) backend: &'a mut B,
    }

    impl<'a, B: SimBackend> IOContext<'a, B> {
        pub fn set<T: Copy>(&mut self, signal: crate::SignalRef, val: T) {
            self.backend.set(signal, val);
        }

        pub fn set_wide(&mut self, signal: crate::SignalRef, val: crate::BigUint) {
            self.backend.set_wide(signal, val);
        }

        pub fn set_four_state(
            &mut self,
            signal: crate::SignalRef,
            val: crate::BigUint,
            mask: crate::BigUint,
        ) {
            self.backend.set_four_state(signal, val, mask);
        }
    }

    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", feature = "experimental-arm64-backend")
    ))]
    pub mod native_backend {
        //! Re-exports for the custom native backend (for testing/integration).
        pub use crate::backend::native::*;
    }

    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", feature = "experimental-arm64-backend")
    ))]
    pub use crate::backend::native::backend::NativeEventRef;
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", feature = "experimental-arm64-backend")
    ))]
    pub use crate::backend::native::{
        AppendedNativeImage, NativeBackend, NativeCodeEntry, NativeImageArchitecture,
        NativeImageContainerError, NativeProgramImage, NativeProgramInstance,
        NativeProgramLoadError, SharedNativeCode,
    };
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", feature = "experimental-arm64-backend")
    ))]
    pub use crate::backend::{NativeDiagnostics, NativeDumpOptions};

    /// Default simulation backend: custom native on x86-64 and opt-in AArch64,
    /// Cranelift elsewhere.
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", feature = "experimental-arm64-backend")
    ))]
    pub type DefaultBackend = NativeBackend;
    #[cfg(not(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", feature = "experimental-arm64-backend")
    )))]
    pub type DefaultBackend = JitBackend;

    pub type DefaultEventRef = <DefaultBackend as SimBackend>::Event;
}

#[cfg(feature = "host-runtime")]
pub use host_api::*;

// Re-exports needed for wasm32 builds
pub use backend::wasm_codegen;

// Public compilation API (available on all targets)
pub use simulator::compile_to_sir;

#[cfg(test)]
mod flatting_tests;
