//! Options of a simulator build.

use super::*;

/// Controls when the tiered backend adopts background-compiled code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierPromotion {
    /// Adopt compiled code at the first scheduler safe point after
    /// background compilation completes. Default.
    Always,
    /// Adopt compiled code only once the interpreter has executed at
    /// least this many evaluation steps. Background compilation still
    /// starts immediately; short simulations that finish before the
    /// threshold simply never pay the adoption cost. `AfterSteps(0)`
    /// behaves like [`TierPromotion::Always`].
    AfterSteps(u64),
    /// Stay interpreted permanently and skip background compilation
    /// entirely — no worker thread is spawned.
    Never,
}

#[derive(Debug, Clone)]
pub struct SimulatorOptions {
    pub four_state: bool,
    /// Per-pass SIRT optimizer flags.
    pub optimize_options: crate::optimizer::OptimizeOptions,
    /// Fine-grained Cranelift backend options.
    pub cranelift_options: crate::backend::CraneliftOptions,
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub x86_options: crate::backend::X86BackendOptions,
    pub trace: crate::debug::TraceOptions,
    pub diagnostics: crate::RuntimeDiagnostics,
    pub diagnostic_policy: crate::DiagnosticPolicy,
    /// Default SystemVerilog time unit and global scheduler precision.
    pub sv_timescale: Option<crate::SvTimescale>,
    /// Return zero from every empty SV array reduction for compatibility.
    pub legacy_empty_reductions: bool,
    /// When true, JIT-compiled functions emit trigger detection code for
    /// edge-based event discovery. Only needed by [`crate::Simulation`].
    pub emit_triggers: bool,
    /// Whether the host may move event signals ([`crate::Simulator::tick`]).
    /// A timed [`crate::Simulation`] has no such API: its compiled
    /// scheduler moves them, so the generated derived-edge units, which
    /// only host-moved edges run, are built only for the events of the
    /// kernels the scheduler leaves to the host.
    pub(crate) host_edges: bool,
    /// Generated derived-edge units the tests run, recorded here.
    #[cfg(all(test, feature = "cranelift", feature = "systemverilog"))]
    pub(crate) derived_edge_units: Option<crate::derived_edge::TestEdgeUnits>,
    /// A rewrite of the optimized SIR before layout, with which tests
    /// build programs no frontend produces yet.
    #[cfg(test)]
    pub(crate) sir_transform: Option<fn(&mut crate::ir::SirProgram)>,
    /// Emit the additional combinational entries needed to reapply foreign
    /// force overrides between procedural store boundaries.
    pub native_force_support: bool,
    /// Generate the compiled scheduler with a host observation point at
    /// every update (see [`SimulatorBuilder::observable_updates`]).
    pub observable_updates: bool,
    /// Cross-check every partitioned settle against one that runs every
    /// partition (see [`SimulatorBuilder::settle_check`]).
    pub settle_check: bool,
    /// Explicit hierarchical observation and native force targets.
    pub signal_access: crate::SignalAccessOptions,
    /// Dead store elimination policy.
    pub dead_store_policy: DeadStorePolicy,
    /// When the tiered backend adopts background-compiled code.
    pub tier_promotion: TierPromotion,
    /// Compile hot execution groups independently on the tiered backend
    /// (native x86-64 and AArch64 hosts), keeping cold work interpreted.
    /// On by default; `false` selects whole-image tiered compilation.
    /// Configurations it does not support (other hosts, parallel lanes,
    /// force targets, deferred promotion, codegen traces) keep whole-image
    /// tiering.
    pub hotspot_jit: bool,
    /// Allow Celox to lower function side effects that Veryl rejects in
    /// `always_ff` for SystemVerilog compatibility.
    pub allow_always_ff_function_effects: bool,
    /// Permit inferred latches in `always_comb`; disabled by default.
    pub allow_inferred_latches: bool,
    /// Where the C functions of DPI-C imports are found.
    pub dpi: crate::DpiSymbols,
    /// Ordered command-line arguments for SystemVerilog plusargs queries.
    pub plusargs: Vec<String>,
    /// Host I/O factory; each simulator receives an independent instance.
    pub file_system: celox_runtime::file_io_backend::FileSystemFactory,
    /// Leave timed initialization pending so a host can install a checkpoint
    /// before any declaration or combinational effect executes.
    pub defer_initialization: bool,
    /// The loop that runs the time slots of a [`crate::Simulation`].
    #[doc(hidden)]
    pub scheduler: crate::SchedulerMode,
}

impl Default for SimulatorOptions {
    fn default() -> Self {
        let opt = crate::optimizer::OptimizeOptions::default();
        Self {
            four_state: false,
            optimize_options: opt,
            cranelift_options: crate::backend::CraneliftOptions::default(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            x86_options: crate::backend::X86BackendOptions::default(),
            trace: Default::default(),
            diagnostics: Default::default(),
            diagnostic_policy: Default::default(),
            sv_timescale: None,
            legacy_empty_reductions: false,
            emit_triggers: false,
            host_edges: true,
            #[cfg(all(test, feature = "cranelift", feature = "systemverilog"))]
            derived_edge_units: None,
            #[cfg(test)]
            sir_transform: None,
            settle_check: false,
            native_force_support: false,
            observable_updates: false,
            signal_access: crate::SignalAccessOptions::default(),
            dead_store_policy: DeadStorePolicy::Off,
            tier_promotion: TierPromotion::Always,
            hotspot_jit: true,
            allow_always_ff_function_effects: false,
            allow_inferred_latches: false,
            dpi: crate::DpiSymbols::default(),
            plusargs: Vec::new(),
            file_system: Default::default(),
            defer_initialization: false,
            scheduler: crate::SchedulerMode::default(),
        }
    }
}
