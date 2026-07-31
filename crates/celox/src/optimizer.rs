use crate::ir::Program;

pub mod coalescing;

/// Cranelift backend optimization level.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CraneliftOptLevel {
    /// No Cranelift-level optimizations.
    None,
    /// Optimize for execution speed (default).
    #[default]
    Speed,
    /// Optimize for both speed and code size.
    SpeedAndSize,
}

#[cfg(not(target_arch = "wasm32"))]
impl CraneliftOptLevel {
    /// Returns the Cranelift settings string for this level.
    pub fn as_cranelift_str(self) -> &'static str {
        match self {
            CraneliftOptLevel::None => "none",
            CraneliftOptLevel::Speed => "speed",
            CraneliftOptLevel::SpeedAndSize => "speed_and_size",
        }
    }
}

/// Register allocator algorithm for the Cranelift backend.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegallocAlgorithm {
    /// Backtracking allocator with range splitting.
    /// Slower compilation but generates better code with fewer spills.
    #[default]
    Backtracking,
    /// Single-pass allocator.
    /// Much faster compilation but generates code with more register spills and moves.
    SinglePass,
}

#[cfg(not(target_arch = "wasm32"))]
impl RegallocAlgorithm {
    /// Returns the Cranelift settings string for this algorithm.
    pub fn as_cranelift_str(self) -> &'static str {
        match self {
            RegallocAlgorithm::Backtracking => "backtracking",
            RegallocAlgorithm::SinglePass => "single_pass",
        }
    }
}

/// Fine-grained Cranelift backend options beyond the optimization level.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy)]
pub struct CraneliftOptions {
    /// Optimization level (default: Speed).
    pub opt_level: CraneliftOptLevel,
    /// Register allocator algorithm (default: Backtracking).
    pub regalloc_algorithm: RegallocAlgorithm,
    /// Enable alias analysis during egraph optimization (default: true).
    /// Only effective when `opt_level` is not `None`.
    pub enable_alias_analysis: bool,
    /// Enable the Cranelift IR verifier (default: true).
    /// Disabling saves compile time at the cost of less validation.
    pub enable_verifier: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for CraneliftOptions {
    fn default() -> Self {
        Self {
            opt_level: CraneliftOptLevel::default(),
            regalloc_algorithm: RegallocAlgorithm::default(),
            enable_alias_analysis: true,
            enable_verifier: true,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl CraneliftOptions {
    /// Fast compilation preset: no optimizations, single-pass regalloc, no verifier.
    pub fn fast_compile() -> Self {
        Self {
            opt_level: CraneliftOptLevel::None,
            regalloc_algorithm: RegallocAlgorithm::SinglePass,
            enable_alias_analysis: false,
            enable_verifier: false,
        }
    }
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

    /// Default Cranelift backend options for this level.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn default_cranelift_options(self) -> CraneliftOptions {
        match self {
            OptLevel::O0 => CraneliftOptions::fast_compile(),
            OptLevel::O1 | OptLevel::O2 => CraneliftOptions::default(),
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
/// use celox::{OptLevel, SirPass, OptimizeOptions};
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
    enabled: crate::HashSet<SirPass>,
    disabled: crate::HashSet<SirPass>,
    max_native_memory_width: usize,
    x86_slp: bool,
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
            enabled: crate::HashSet::default(),
            disabled: crate::HashSet::default(),
            max_native_memory_width: if cfg!(target_arch = "x86_64") {
                128
            } else {
                64
            },
            x86_slp: cfg!(target_arch = "x86_64"),
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

    /// Enable or disable target-owned x86 SLP selection after scalar MIR
    /// optimization. This is independent of SIR memory coalescing width so
    /// profitability can be measured without conflating the two transforms.
    pub fn with_x86_slp(mut self, enable: bool) -> Self {
        self.x86_slp = enable;
        self
    }

    pub fn x86_slp_enabled(&self) -> bool {
        self.x86_slp
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

pub trait ProgramPass {
    fn name(&self) -> &'static str;
    fn run(&self, program: &mut Program, options: &PassOptions);
}

#[derive(Default)]
pub struct PassManager {
    passes: Vec<Box<dyn ProgramPass>>,
}

impl PassManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_pass<P>(&mut self, pass: P)
    where
        P: ProgramPass + 'static,
    {
        self.passes.push(Box::new(pass));
    }

    pub fn run(&self, program: &mut Program, options: &PassOptions) {
        for pass in &self.passes {
            let _ = pass.name();
            pass.run(program, options);
        }
    }
}

pub fn optimize(program: &mut Program, four_state: bool, optimize_options: &OptimizeOptions) {
    optimize_impl(program, four_state, optimize_options, false);
}

pub(crate) fn optimize_preserving_element_storage(
    program: &mut Program,
    four_state: bool,
    optimize_options: &OptimizeOptions,
) {
    optimize_impl(program, four_state, optimize_options, true);
}

fn optimize_impl(
    program: &mut Program,
    four_state: bool,
    optimize_options: &OptimizeOptions,
    preserve_element_storage_layout: bool,
) {
    let mut manager = PassManager::new();
    manager.add_pass(coalescing::CoalescingPass);
    manager.run(
        program,
        &PassOptions {
            four_state,
            optimize_options: optimize_options.clone(),
            preserve_element_storage_layout,
            ..PassOptions::default()
        },
    );
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
