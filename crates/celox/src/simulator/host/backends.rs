//! Constructors and conversions of a [`Simulator`] for a specific backend.

use super::*;

#[cfg(feature = "cranelift")]
use crate::backend::{JitBackend, SharedJitCode};
// ── JitBackend-specific methods ──────────────────────────────────────
impl Simulator {
    pub fn builder<'a>(code: &'a str, top: &'a str) -> SimulatorBuilder<'a, Simulator> {
        SimulatorBuilder::<Simulator>::new(code, top)
    }

    pub fn from_sources<'a>(
        sources: Vec<(&'a str, &'a std::path::Path)>,
        top: &'a str,
    ) -> SimulatorBuilder<'a, Simulator> {
        SimulatorBuilder::<Simulator>::from_sources(sources, top)
    }

    /// Low-level adapter hook for an elaborated external frontend artifact.
    ///
    /// Frontend crates should wrap this with a constructor named for their
    /// own artifact type instead of exposing
    /// [`celox_frontend_sdk::FrontendArtifact`] to applications.
    pub fn from_frontend(
        artifact: celox_frontend_sdk::FrontendArtifact,
    ) -> SimulatorBuilder<'static, Simulator> {
        SimulatorBuilder::<Simulator>::from_frontend(artifact)
    }

    /// Low-level adapter hook for a Veryl testbench around an external module.
    pub fn from_frontend_with_testbench<'a>(
        artifact: celox_frontend_sdk::FrontendArtifact,
        sources: Vec<(&'a str, &'a std::path::Path)>,
        top: &'a str,
    ) -> SimulatorBuilder<'a, Simulator> {
        SimulatorBuilder::<Simulator>::from_frontend_with_testbench(artifact, sources, top)
    }

    /// Build a simulator directly from SystemVerilog sources.
    #[cfg(feature = "systemverilog")]
    pub fn from_sv_sources<'a>(
        sources: Vec<(&'a str, &'a std::path::Path)>,
        top: &'a str,
    ) -> SimulatorBuilder<'a, Simulator> {
        SimulatorBuilder::<Simulator>::from_sv_sources(sources, top)
    }

    /// Build a mixed Veryl/SystemVerilog hierarchy with a top in either language.
    #[cfg(feature = "systemverilog")]
    pub fn from_mixed_sources<'a>(
        sources: Vec<(&'a str, &'a std::path::Path)>,
        sv_sources: Vec<(&'a str, &'a std::path::Path)>,
        top: &'a str,
    ) -> SimulatorBuilder<'a, Simulator> {
        SimulatorBuilder::<Simulator>::from_mixed_sources(sources, sv_sources, top)
    }
}

#[cfg(feature = "cranelift")]
impl Simulator<JitBackend> {
    /// Returns the shared compiled JIT code, allowing it to be reused
    /// for creating additional simulator instances without recompilation.
    pub fn shared_code(&self) -> Arc<SharedJitCode> {
        self.backend.shared_code()
    }

    /// Consume the simulator and return the inner JIT backend.
    pub fn into_backend(self) -> JitBackend {
        self.backend
    }
}

#[cfg(feature = "wasm-runtime")]
impl Simulator<crate::backend::wasm_runtime::WasmBackend> {
    /// Consume the simulator and return the inner Wasmtime backend.
    pub fn into_backend(self) -> crate::backend::wasm_runtime::WasmBackend {
        self.backend
    }
}

impl Simulator<crate::backend::TieredBackend> {
    /// Counts for incremental JIT execution, or `None` for whole-image tiering.
    #[cfg(hotspot_jit)]
    pub fn hotspot_stats(&self) -> Option<crate::backend::HotspotStats> {
        self.backend.hotspot_stats()
    }

    /// Whether this tiered simulation has adopted its compiled tier.
    ///
    /// False while background compilation is still running or after it
    /// failed (the interpreter then remains the permanent tier).
    pub fn is_compiled(&self) -> bool {
        self.backend.is_compiled()
    }

    /// Return deterministic tier lifecycle and evaluation measurements.
    ///
    /// The snapshot contains counts rather than ad-hoc wall-clock samples;
    /// use a statistical benchmark harness to measure elapsed time.
    pub fn tiered_execution_stats(&self) -> crate::TieredExecutionStats {
        self.backend.execution_stats()
    }

    /// Start opt-in wall-clock measurement from workload execution to
    /// promotion. Ordinary tiered simulation does not read the host clock.
    pub fn start_tiered_execution_timing(&mut self) {
        self.backend.start_execution_timing();
    }

    /// Stop tiered execution timing and return the promotion interval.
    pub fn finish_tiered_execution_timing(&mut self) -> Option<crate::TieredExecutionTiming> {
        self.backend.finish_execution_timing()
    }

    /// Why promotion has not happened yet, for diagnostics.
    pub fn promotion_error(&self) -> Option<&SimulatorError> {
        self.backend.promotion_error()
    }

    /// Request cancellation of background compilation.
    ///
    /// The simulation stays on the interpreter permanently and
    /// [`Simulator::promotion_error`] reports the cancellation. Returns
    /// whether a background compilation was still pending.
    pub fn cancel_background_compilation(&mut self) -> bool {
        self.backend.cancel_background_compilation()
    }

    /// Consume the simulator and return the inner tiered backend.
    pub fn into_backend(self) -> crate::backend::TieredBackend {
        self.backend
    }
}

#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
impl Simulator<NativeBackend> {
    /// Returns the shared compiled native code, allowing it to be reused
    /// for creating additional simulator instances without recompilation.
    pub fn shared_code(&self) -> Arc<SharedNativeCode> {
        self.backend.shared_code()
    }

    /// Start measuring time spent inside generated native simulator functions.
    ///
    /// The measurement is disabled by default, so ordinary simulation does not
    /// read the host clock. Native tick loops are measured once per host-side
    /// batch rather than once per simulated tick.
    pub fn start_native_execution_timing(&mut self) {
        self.backend.start_execution_timing();
    }

    /// Stop native execution timing and return the accumulated measurement.
    pub fn finish_native_execution_timing(
        &mut self,
    ) -> Option<crate::native_backend::NativeExecutionTiming> {
        self.backend.finish_execution_timing()
    }

    /// Create a simulator from pre-compiled shared native code.
    pub fn from_shared(shared: Arc<SharedNativeCode>, program: crate::ir::OptimizedSir) -> Self {
        let backend = NativeBackend::from_shared(shared);
        let mut sim = Self::with_backend_and_program(backend, program.into_runtime(), vec![]);
        sim.apply_initial_values();
        sim
    }

    /// Consume the simulator and return the inner native backend.
    pub fn into_backend(self) -> NativeBackend {
        self.backend
    }
}
