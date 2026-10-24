#![cfg_attr(
    test,
    allow(
        clippy::disallowed_macros,
        reason = "thread-local instrumentation exists only in unit tests; non-test library builds enforce the ban"
    )
)]
mod backend;
#[cfg(all(feature = "arm64-codegen", feature = "x86_64-codegen"))]
compile_error!("arm64-codegen and x86_64-codegen cannot be enabled together");
#[cfg(feature = "host-runtime")]
mod comb_activation;
#[cfg(feature = "host-runtime")]
mod component;
#[cfg(feature = "host-runtime")]
pub use component::{
    InjectedCall, InjectedComponentHandler, InjectedComponents, InjectedHook, InjectedNamedValue,
    InjectedPort, InjectedResult, InjectedValue, register_static_component,
    register_static_component_manifest,
};
mod debug;
#[cfg(feature = "host-runtime")]
mod derived_edge;
mod edge_plans;
pub use edge_plans::{
    EdgePlanDomain, EdgePlanEvent, EdgePlanSignal, EdgePlans, edge_plans, event_addresses,
};
mod diagnostics;
// Without a host runtime nothing builds simulators, so nothing links DPI-C.
#[cfg_attr(not(feature = "host-runtime"), allow(dead_code))]
mod dpi;
mod interpreter;
mod ir;
mod optimizer;
mod parser;
pub(crate) mod portable;
// Builds without the host runtime run every slot in the reference loop.
#[cfg(feature = "host-runtime")]
mod scheduler;
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
pub use celox_design::{
    AssertionDeferral, AssertionDirective, AssertionOutcome, AssertionSite, DomainKind,
    ElaboratedDesign, EventTopology, ProcessOrigin, ReportLocation, RuntimeSchema, Severity,
};
pub use celox_frontend_core::FrontendArtifactError;
pub use celox_frontend_sdk as frontend_sdk;
pub use celox_frontend_sdk::FrontendArtifact;
/// Warnings of the SystemVerilog frontend, in [`CompilationWarning::SystemVerilog`].
#[cfg(feature = "systemverilog")]
pub use celox_frontend_sv::{
    FrontendWarning as SvFrontendWarning, FrontendWarningKind as SvFrontendWarningKind,
    WarningLocation as SvWarningLocation,
};
pub use celox_frontend_veryl::{FrontendDiagnostic, LoweringPhase, ParserError};
pub use celox_runtime::{
    BaselineRecord, DesignReflection, NbaRecord, ProcessRecord, ReflectionScope, ReflectionScopeId,
    ReflectionSignal, ReflectionSignalId, ScheduleRecord, ScheduledEvent, SignalDirection,
    StateDifference, StateFile, StateFileError, StateObject, StateRole, WaiterRecord,
    format_state_value,
};
#[doc(hidden)]
pub use celox_runtime::{KernelSchedule, SchedulerMode, SchedulerStats};
pub use celox_slt::scheduler::SchedulerError;
pub use celox_state_layout::STATE_HEADER_SIZE;
pub use debug::{CompilationTrace, NativeProfileBlock, TraceOptions};
pub use diagnostics::RuntimeDiagnostics;
pub use dpi::{DpiError, DpiSymbols};
pub(crate) use fxhash::FxHashMap as HashMap;
pub(crate) use fxhash::FxHashSet as HashSet;
pub use interpreter::{
    InterpError, InterpMachine, ResolvedAccess, StoreSnapshot, UnitExit, execute_unit,
};
pub use ir::{
    AbsoluteAddr, AddrLookupError, FrontendLookup, InstancePath, LaidOutProgram, OptimizedSir,
    PortTypeKind, RuntimeDesign, RuntimeErrorInfo, RuntimeInstance, RuntimeProgram,
    RuntimeVariable, SignalRef, SirProgram, SourceAddr, SourceVarId, UnoptimizedSir, VariableInfo,
    VariableKind,
};
pub use num_bigint::BigUint;
pub use optimizer::OptLevel;
pub use optimizer::OptimizeOptions;
pub use optimizer::ParallelPartition;
pub use optimizer::SirDiagnostics;
pub use optimizer::SirPass;
pub use simulator::render_diagnostic;
pub use simulator::{CodegenError, CompilationWarning, SimulatorError, SimulatorErrorKind};
pub use veryl_metadata::{ClockType, ResetType};

/// Largest worker-thread count accepted by `SimulatorBuilder::threads`.
pub const MAX_SIMULATION_THREADS: u32 = 64;

#[cfg(feature = "host-runtime")]
mod host_api {
    use crate::SimBackend;
    pub use crate::backend::tiered::TieredEventRef;
    pub use crate::backend::wasm_runtime::WasmBackend;
    pub use crate::backend::{
        CraneliftDiagnostics, CraneliftOptLevel, CraneliftOptions, EventRef, InterpBackend,
        JitBackend, RegallocAlgorithm, SharedJitCode, TieredBackend, TieredExecutionStats,
        TieredExecutionTier, TieredExecutionTiming, TieredPromotionStatus,
    };
    pub use crate::debug::CompilationTraceResult;
    pub use crate::diagnostics::DiagnosticsOptions;
    pub use crate::simulation::{Simulation, SimulationCheckpoint, SlotObserver};
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub use crate::simulator::NativeCompilation;
    pub use crate::simulator::{
        Checkpoint, CheckpointError, DeadStorePolicy, DumpError, InstanceHierarchy, NamedEvent,
        NamedSignal, Report, RuntimeEvent, RuntimeEventDrain, RuntimeFormatContext, Simulator,
        SimulatorBuilder, SimulatorOptions, StateError, StateImage, StateMismatch, StateSchema,
        TierPromotion,
    };
    pub use crate::testbench::{AssertionResult, SourceLocation, TestResult, TestResultDetailed};
    pub use celox_macros::veryl_test;
    pub use celox_runtime::{
        BackendExecutor, DerivedTickPlan, DerivedTickPlans, DerivedTickScratch, Edge, EdgeOutcome,
        EventEdge, EventEdges, Halt, SignalWrite, VcdSignalDesc, VcdStatistics, VcdWriter,
        move_signal, run_edge, run_edges, run_tick, set_level,
    };

    pub struct IOContext<'a, B: SimBackend = DefaultBackend> {
        pub(crate) backend: &'a mut B,
        /// Writes a timed simulation observes one at a time; empty
        /// otherwise.
        pub(crate) watch: &'a mut celox_runtime::HostWatch,
    }

    impl<'a, B: SimBackend> IOContext<'a, B> {
        fn write(&mut self, signal: crate::SignalRef, write: impl FnOnce(&mut B)) {
            if self.watch.is_empty() {
                write(self.backend);
            } else {
                self.watch.write(self.backend, signal, write);
            }
        }

        pub fn set<T: Copy>(&mut self, signal: crate::SignalRef, val: T) {
            self.write(signal, |backend| backend.set(signal, val));
        }

        pub fn set_wide(&mut self, signal: crate::SignalRef, val: crate::BigUint) {
            self.write(signal, |backend| backend.set_wide(signal, val));
        }

        /// Read the current stored payload and X/Z mask without evaluating
        /// combinational logic. Includes earlier writes in this `modify` call.
        pub fn get_four_state(&self, signal: crate::SignalRef) -> (crate::BigUint, crate::BigUint) {
            self.backend.get_four_state(signal)
        }

        pub fn set_four_state(
            &mut self,
            signal: crate::SignalRef,
            val: crate::BigUint,
            mask: crate::BigUint,
        ) {
            self.write(signal, |backend| backend.set_four_state(signal, val, mask));
        }
    }

    /// State access of a [`SlotObserver`] inside a time slot: reads see
    /// the slot's state so far, writes are blocking writes of the host at
    /// the slot's time.
    pub struct SlotIo<'a, B: SimBackend = DefaultBackend> {
        io: IOContext<'a, B>,
        wrote: bool,
    }

    impl<'a, B: SimBackend> SlotIo<'a, B> {
        pub(crate) fn new(io: IOContext<'a, B>) -> Self {
            Self { io, wrote: false }
        }

        pub(crate) fn wrote(&self) -> bool {
            self.wrote
        }

        /// The stored value, with X/Z bits as their payload.
        pub fn get(&self, signal: crate::SignalRef) -> crate::BigUint {
            self.io.backend.get(signal)
        }

        /// The stored payload and X/Z mask.
        pub fn get_four_state(&self, signal: crate::SignalRef) -> (crate::BigUint, crate::BigUint) {
            self.io.get_four_state(signal)
        }

        pub fn set<T: Copy>(&mut self, signal: crate::SignalRef, val: T) {
            self.wrote = true;
            self.io.set(signal, val);
        }

        pub fn set_wide(&mut self, signal: crate::SignalRef, val: crate::BigUint) {
            self.wrote = true;
            self.io.set_wide(signal, val);
        }

        pub fn set_four_state(
            &mut self,
            signal: crate::SignalRef,
            val: crate::BigUint,
            mask: crate::BigUint,
        ) {
            self.wrote = true;
            self.io.set_four_state(signal, val, mask);
        }
    }

    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub mod native_backend {
        //! Re-exports for the custom native backend (for testing/integration).
        pub use crate::backend::native::*;
    }

    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub use crate::backend::native::backend::NativeEventRef;
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub use crate::backend::native::{
        AppendedNativeImage, NativeBackend, NativeCodeEntry, NativeImageArchitecture,
        NativeImageContainerError, NativeProgramImage, NativeProgramInstance,
        NativeProgramLoadError, NativeSignalIdentity, SharedNativeCode,
    };
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub use crate::backend::{NativeDiagnostics, NativeDumpOptions};

    /// Default simulation backend: custom native when it matches the compilation
    /// target, and Cranelift when a host-side cross-codegen feature is enabled.
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    pub type DefaultBackend = NativeBackend;
    #[cfg(not(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    )))]
    pub type DefaultBackend = JitBackend;

    pub type DefaultEventRef = <DefaultBackend as SimBackend>::Event;
}

#[cfg(feature = "host-runtime")]
pub use host_api::*;

// Re-exports needed for wasm32 builds
pub use backend::wasm_codegen;

// Public compilation API (available on all targets)
pub use simulator::{
    compile_frontend_to_sir, compile_to_sir, compile_to_sir_with_diagnostic_policy,
    compile_to_sir_with_file_dependencies, compile_to_sir_with_latch_policy,
};
#[cfg(feature = "systemverilog")]
pub use simulator::{
    compile_mixed_to_sir, compile_mixed_to_sir_with_file_dependencies, compile_sv_to_sir,
};

#[cfg(test)]
mod flatting_tests;

mod diagnostic_policy;
pub use diagnostic_policy::{DiagnosticLevel, DiagnosticPolicy};

mod signal_access;
pub use signal_access::SignalAccessOptions;

mod timescale;
pub use timescale::SvTimescale;
