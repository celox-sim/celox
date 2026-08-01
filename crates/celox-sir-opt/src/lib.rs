//! Backend-independent SIR optimization policy and pass pipeline.

pub type HashMap<K, V> = fxhash::FxHashMap<K, V>;
pub type HashSet<K> = fxhash::FxHashSet<K>;

/// Source-independent SIR and design vocabulary specialized to semantic state
/// addresses. It deliberately exposes no frontend or physical-layout type.
pub mod ir {
    pub use celox_design::{
        BinaryOp, DomainKind, InstanceId, RegionedAbsoluteAddrBase, RuntimeSchema,
        SPARSE_WORKING_REGION, STABLE_REGION, StateAddr, StateObjectId, TriggerIdWithKind, UnaryOp,
        VarAtomBase, WORKING_REGION,
    };
    pub use celox_sir::*;

    pub type AbsoluteAddr = celox_design::StateAddr;
    pub type RegionedAbsoluteAddr = celox_design::RegionedStateAddr;
    pub type SirProgram = celox_sir::SirProgram<AbsoluteAddr, RegionedAbsoluteAddr>;

    pub mod cfg {
        pub use celox_sir::cfg::*;
    }

    pub mod verify {
        pub use celox_sir::verify::*;
    }
}

/// Mutable optimization view over source-independent compiler state.
/// Frontend lookup tables and testbench source cannot enter this crate.
pub struct OptimizationContext<'a> {
    pub sir: &'a mut ir::SirProgram,
    pub design: &'a celox_design::ElaboratedDesign<ir::AbsoluteAddr>,
    pub runtime_schema: &'a celox_design::RuntimeSchema<ir::AbsoluteAddr>,
    pub layout_requirements: &'a mut celox_state_layout::LayoutRequirements<ir::AbsoluteAddr>,
}

impl OptimizationContext<'_> {
    pub fn variable_metadata(
        &self,
        address: &ir::AbsoluteAddr,
    ) -> Option<&celox_design::VariableMetadata> {
        self.design.state_objects.get(address)
    }
}

pub mod timing {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn now() -> std::time::Instant {
        std::time::Instant::now()
    }

    #[cfg(target_arch = "wasm32")]
    pub fn now() -> WasmInstant {
        WasmInstant
    }

    #[cfg(target_arch = "wasm32")]
    #[derive(Clone, Copy)]
    pub struct WasmInstant;

    #[cfg(target_arch = "wasm32")]
    impl WasmInstant {
        pub fn elapsed(&self) -> std::time::Duration {
            std::time::Duration::ZERO
        }
    }
}

/// Cost-model threshold for preferring chunked memory shifts.
const MEM_SHIFT_THRESHOLD: usize = 4;

pub mod coalescing;
mod memory_contract;
pub use memory_contract::verify_memory_offset_contract;

trait SirProgramPass {
    fn run(&self, program: &mut OptimizationContext<'_>, options: &PassOptions);
}

pub fn optimize(
    program: &mut OptimizationContext<'_>,
    four_state: bool,
    optimize_options: &OptimizeOptions,
    preserve_element_storage_layout: bool,
) {
    let pass = coalescing::CoalescingPass;
    pass.run(
        program,
        &PassOptions {
            four_state,
            optimize_options: optimize_options.clone(),
            preserve_element_storage_layout,
            ..PassOptions::default()
        },
    );
}

// ── OptLevel / SirPass / OptimizeOptions ────────────────────────────

/// Optimization level presets, analogous to GCC's `-O` flags.
///
/// Each level sets defaults for SIR passes, Cranelift backend options,
/// and dead store elimination policy. Individual passes can be overridden
/// via [`OptimizeOptions::enable`] / [`OptimizeOptions::disable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OptLevel {
    /// No semantic SIR optimizations. The compatibility
    /// [`SirPass::TailCallSplit`] selector is consumed only by Cranelift.
    /// Cranelift: `fast_compile()`. DSE: Off.
    O0,
    /// Production-default SIR optimizations enabled, including conservative
    /// two-state control-flow recovery for profitable Mux regions.
    /// Cranelift: Speed / Backtracking. DSE: Off.
    #[default]
    O1,
    /// Production-default SIR optimizations + DSE(`PreserveTopPorts`),
    /// including conservative two-state control-flow recovery for profitable
    /// Mux regions.
    /// Cranelift: Speed / Backtracking.
    O2,
}

impl OptLevel {
    /// Returns whether a given SIR pass is enabled by default at this level.
    pub fn default_enabled(self, pass: SirPass) -> bool {
        match self {
            OptLevel::O0 => matches!(pass, SirPass::TailCallSplit),
            OptLevel::O1 | OptLevel::O2 => true,
        }
    }

    /// Parse from string (for NAPI/CLI).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "O0" | "o0" => Some(Self::O0),
            "O1" | "o1" => Some(Self::O1),
            "O2" | "o2" => Some(Self::O2),
            _ => None,
        }
    }

    /// String representation.
    pub fn as_str(self) -> &'static str {
        match self {
            OptLevel::O0 => "O0",
            OptLevel::O1 => "O1",
            OptLevel::O2 => "O2",
        }
    }
}

/// Individual SIR optimization passes that can be toggled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SirPass {
    StoreLoadForwarding,
    ControlFlowSimplify,
    HoistCommonBranchLoads,
    BitExtractPeephole,
    OptimizeBlocks,
    SplitWideCommits,
    CommitSinking,
    InlineCommitForwarding,
    EliminateDeadWorkingStores,
    Reschedule,
    CoalesceStores,
    Gvn,
    ConcatFolding,
    XorChainFolding,
    VectorizeConcat,
    MaskedArrayAny,
    CircularPriority,
    IndexedStoreRecovery,
    BranchifyMux,
    SplitCoalescedStores,
    PartialForward,
    IdentityStoreBypass,
    /// Compatibility selector for Cranelift oversized-function planning.
    /// This is not a SIR transform and is consumed at the backend boundary.
    TailCallSplit,
}

impl SirPass {
    /// All pass variants in definition order.
    pub const ALL: &[SirPass] = &[
        SirPass::StoreLoadForwarding,
        SirPass::ControlFlowSimplify,
        SirPass::HoistCommonBranchLoads,
        SirPass::BitExtractPeephole,
        SirPass::OptimizeBlocks,
        SirPass::SplitWideCommits,
        SirPass::CommitSinking,
        SirPass::InlineCommitForwarding,
        SirPass::EliminateDeadWorkingStores,
        SirPass::Reschedule,
        SirPass::CoalesceStores,
        SirPass::Gvn,
        SirPass::ConcatFolding,
        SirPass::XorChainFolding,
        SirPass::VectorizeConcat,
        SirPass::MaskedArrayAny,
        SirPass::CircularPriority,
        SirPass::IndexedStoreRecovery,
        SirPass::BranchifyMux,
        SirPass::SplitCoalescedStores,
        SirPass::PartialForward,
        SirPass::IdentityStoreBypass,
        SirPass::TailCallSplit,
    ];

    /// Snake_case string representation (for NAPI/TS serialization).
    pub fn as_str(self) -> &'static str {
        match self {
            SirPass::StoreLoadForwarding => "store_load_forwarding",
            SirPass::ControlFlowSimplify => "control_flow_simplify",
            SirPass::HoistCommonBranchLoads => "hoist_common_branch_loads",
            SirPass::BitExtractPeephole => "bit_extract_peephole",
            SirPass::OptimizeBlocks => "optimize_blocks",
            SirPass::SplitWideCommits => "split_wide_commits",
            SirPass::CommitSinking => "commit_sinking",
            SirPass::InlineCommitForwarding => "inline_commit_forwarding",
            SirPass::EliminateDeadWorkingStores => "eliminate_dead_working_stores",
            SirPass::Reschedule => "reschedule",
            SirPass::CoalesceStores => "coalesce_stores",
            SirPass::Gvn => "gvn",
            SirPass::ConcatFolding => "concat_folding",
            SirPass::XorChainFolding => "xor_chain_folding",
            SirPass::VectorizeConcat => "vectorize_concat",
            SirPass::MaskedArrayAny => "masked_array_any",
            SirPass::CircularPriority => "circular_priority",
            SirPass::IndexedStoreRecovery => "indexed_store_recovery",
            SirPass::BranchifyMux => "branchify_mux",
            SirPass::SplitCoalescedStores => "split_coalesced_stores",
            SirPass::PartialForward => "partial_forward",
            SirPass::IdentityStoreBypass => "identity_store_bypass",
            SirPass::TailCallSplit => "tail_call_split",
        }
    }

    /// Parse from snake_case string (for NAPI/CLI).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "store_load_forwarding" => Some(SirPass::StoreLoadForwarding),
            "control_flow_simplify" => Some(SirPass::ControlFlowSimplify),
            "hoist_common_branch_loads" => Some(SirPass::HoistCommonBranchLoads),
            "bit_extract_peephole" => Some(SirPass::BitExtractPeephole),
            "optimize_blocks" => Some(SirPass::OptimizeBlocks),
            "split_wide_commits" => Some(SirPass::SplitWideCommits),
            "commit_sinking" => Some(SirPass::CommitSinking),
            "inline_commit_forwarding" => Some(SirPass::InlineCommitForwarding),
            "eliminate_dead_working_stores" => Some(SirPass::EliminateDeadWorkingStores),
            "reschedule" => Some(SirPass::Reschedule),
            "coalesce_stores" => Some(SirPass::CoalesceStores),
            "gvn" => Some(SirPass::Gvn),
            "concat_folding" => Some(SirPass::ConcatFolding),
            "xor_chain_folding" => Some(SirPass::XorChainFolding),
            "vectorize_concat" => Some(SirPass::VectorizeConcat),
            "masked_array_any" => Some(SirPass::MaskedArrayAny),
            "circular_priority" => Some(SirPass::CircularPriority),
            "indexed_store_recovery" => Some(SirPass::IndexedStoreRecovery),
            "branchify_mux" => Some(SirPass::BranchifyMux),
            "split_coalesced_stores" => Some(SirPass::SplitCoalescedStores),
            "partial_forward" => Some(SirPass::PartialForward),
            "identity_store_bypass" => Some(SirPass::IdentityStoreBypass),
            "tail_call_split" => Some(SirPass::TailCallSplit),
            _ => None,
        }
    }
}

/// Controls which SIR optimization passes are enabled.
///
/// Built from an [`OptLevel`] preset, with optional per-pass overrides.
///
/// # Examples
///
/// ```
/// use celox_sir_opt::{OptLevel, SirPass, OptimizeOptions};
///
/// // Production defaults enabled, including BranchifyMux.
/// let opts = OptimizeOptions::default();
/// assert!(opts.is_enabled(SirPass::Gvn));
/// assert!(opts.is_enabled(SirPass::BranchifyMux));
///
/// // O0 with one pass selectively enabled
/// let opts = OptimizeOptions::new(OptLevel::O0)
///     .enable(SirPass::Gvn);
/// assert!(opts.is_enabled(SirPass::Gvn));
/// assert!(!opts.is_enabled(SirPass::Reschedule));
/// ```
#[derive(Debug, Clone)]
pub struct OptimizeOptions {
    opt_level: OptLevel,
    enabled: HashSet<SirPass>,
    disabled: HashSet<SirPass>,
    max_native_memory_width: usize,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self::new(OptLevel::default())
    }
}

impl OptimizeOptions {
    /// Create options from an optimization level preset.
    pub fn new(level: OptLevel) -> Self {
        Self {
            opt_level: level,
            enabled: HashSet::default(),
            disabled: HashSet::default(),
            max_native_memory_width: if cfg!(target_arch = "x86_64") {
                128
            } else {
                64
            },
        }
    }

    /// All production-default passes enabled (equivalent to `OptLevel::O1`).
    pub fn all() -> Self {
        Self::new(OptLevel::O1)
    }

    /// All passes disabled except TailCallSplit (equivalent to `OptLevel::O0`).
    pub fn none() -> Self {
        Self::new(OptLevel::O0)
    }

    /// Enable a pass regardless of the OptLevel default.
    pub fn enable(mut self, pass: SirPass) -> Self {
        self.disabled.remove(&pass);
        self.enabled.insert(pass);
        self
    }

    /// Disable a pass regardless of the OptLevel default.
    pub fn disable(mut self, pass: SirPass) -> Self {
        self.enabled.remove(&pass);
        self.disabled.insert(pass);
        self
    }

    /// Set the largest contiguous Store represented as one SIR value.
    ///
    /// 64 preserves scalar word-sized placement. 128 exposes one x86 vector
    /// to target SLP while wider stores are still split before lowering.
    pub fn with_max_native_memory_width(mut self, width: usize) -> Self {
        assert!(
            matches!(width, 64 | 128),
            "coalesced Store width must be 64 or 128 bits"
        );
        self.max_native_memory_width = width;
        self
    }

    pub fn max_native_memory_width(&self) -> usize {
        self.max_native_memory_width
    }

    /// Query whether a specific pass is active.
    pub fn is_enabled(&self, pass: SirPass) -> bool {
        if self.enabled.contains(&pass) {
            return true;
        }
        if self.disabled.contains(&pass) {
            return false;
        }
        self.opt_level.default_enabled(pass)
    }

    /// Returns true if any pass other than TailCallSplit is enabled.
    pub fn any_enabled(&self) -> bool {
        SirPass::ALL
            .iter()
            .any(|&p| p != SirPass::TailCallSplit && self.is_enabled(p))
    }

    /// The base optimization level.
    pub fn opt_level(&self) -> OptLevel {
        self.opt_level
    }
}

#[derive(Debug, Clone)]
pub struct PassOptions {
    pub max_inflight_loads: usize,
    pub four_state: bool,
    pub optimize_options: OptimizeOptions,
    /// Preserve source array element boundaries for a backend layout that
    /// stores each element in its own naturally sized scalar slot.
    pub preserve_element_storage_layout: bool,
}

impl Default for PassOptions {
    fn default() -> Self {
        Self {
            max_inflight_loads: 8,
            four_state: false,
            optimize_options: OptimizeOptions::default(),
            preserve_element_storage_layout: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OptLevel, OptimizeOptions, SirPass};

    #[test]
    fn branchify_is_enabled_by_optimization_presets() {
        assert!(OptimizeOptions::new(OptLevel::O1).is_enabled(SirPass::BranchifyMux));
        assert!(OptimizeOptions::new(OptLevel::O2).is_enabled(SirPass::BranchifyMux));
        assert!(OptimizeOptions::all().is_enabled(SirPass::BranchifyMux));
    }

    #[test]
    fn control_flow_simplify_is_a_production_default() {
        assert!(OptimizeOptions::new(OptLevel::O1).is_enabled(SirPass::ControlFlowSimplify));
        assert!(OptimizeOptions::new(OptLevel::O2).is_enabled(SirPass::ControlFlowSimplify));
        assert!(!OptimizeOptions::new(OptLevel::O0).is_enabled(SirPass::ControlFlowSimplify));
    }

    #[test]
    fn masked_array_any_is_cli_addressable_and_a_production_default() {
        assert_eq!(
            SirPass::parse("masked_array_any"),
            Some(SirPass::MaskedArrayAny)
        );
        assert_eq!(SirPass::MaskedArrayAny.as_str(), "masked_array_any");
        assert!(OptimizeOptions::new(OptLevel::O1).is_enabled(SirPass::MaskedArrayAny));
        assert!(OptimizeOptions::new(OptLevel::O2).is_enabled(SirPass::MaskedArrayAny));
        assert!(!OptimizeOptions::new(OptLevel::O0).is_enabled(SirPass::MaskedArrayAny));
    }

    #[test]
    fn circular_priority_is_cli_addressable_and_a_production_default() {
        assert_eq!(
            SirPass::parse("circular_priority"),
            Some(SirPass::CircularPriority)
        );
        assert_eq!(SirPass::CircularPriority.as_str(), "circular_priority");
        assert!(OptimizeOptions::new(OptLevel::O1).is_enabled(SirPass::CircularPriority));
        assert!(OptimizeOptions::new(OptLevel::O2).is_enabled(SirPass::CircularPriority));
        assert!(!OptimizeOptions::new(OptLevel::O0).is_enabled(SirPass::CircularPriority));
    }

    #[test]
    fn indexed_store_recovery_is_cli_addressable_and_a_production_default() {
        assert_eq!(
            SirPass::parse("indexed_store_recovery"),
            Some(SirPass::IndexedStoreRecovery)
        );
        assert_eq!(
            SirPass::IndexedStoreRecovery.as_str(),
            "indexed_store_recovery"
        );
        assert!(OptimizeOptions::new(OptLevel::O1).is_enabled(SirPass::IndexedStoreRecovery));
        assert!(OptimizeOptions::new(OptLevel::O2).is_enabled(SirPass::IndexedStoreRecovery));
        assert!(!OptimizeOptions::new(OptLevel::O0).is_enabled(SirPass::IndexedStoreRecovery));
        assert!(
            !OptimizeOptions::new(OptLevel::O2)
                .disable(SirPass::IndexedStoreRecovery)
                .is_enabled(SirPass::IndexedStoreRecovery)
        );
    }
}
