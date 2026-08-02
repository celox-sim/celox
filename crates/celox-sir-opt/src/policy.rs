use crate::HashSet;

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

// Keep the public selector, its serialized spelling, and `ALL` in one source
// of truth. A pass added here is automatically visible to CLI/NAPI parsing.
macro_rules! define_sir_passes {
    ($( $(#[$meta:meta])* $variant:ident => $name:literal ),+ $(,)?) => {
        /// Individual SIR optimization passes that can be toggled.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum SirPass {
            $( $(#[$meta])* $variant, )+
        }

        impl SirPass {
            /// All pass variants in definition order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// Snake_case string representation (for NAPI/TS serialization).
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }

            /// Parse from snake_case string (for NAPI/CLI).
            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

define_sir_passes! {
    StoreLoadForwarding => "store_load_forwarding",
    ControlFlowSimplify => "control_flow_simplify",
    HoistCommonBranchLoads => "hoist_common_branch_loads",
    BitExtractPeephole => "bit_extract_peephole",
    OptimizeBlocks => "optimize_blocks",
    SplitWideCommits => "split_wide_commits",
    CommitSinking => "commit_sinking",
    InlineCommitForwarding => "inline_commit_forwarding",
    EliminateDeadWorkingStores => "eliminate_dead_working_stores",
    Reschedule => "reschedule",
    CoalesceStores => "coalesce_stores",
    Gvn => "gvn",
    ConcatFolding => "concat_folding",
    XorChainFolding => "xor_chain_folding",
    VectorizeConcat => "vectorize_concat",
    MaskedArrayAny => "masked_array_any",
    CircularPriority => "circular_priority",
    IndexedStoreRecovery => "indexed_store_recovery",
    GuardedRegionSinking => "guarded_region_sinking",
    LoopIdiom => "loop_idiom",
    PackedScatterStore => "packed_scatter_store",
    SparseCaseDispatch => "sparse_case_dispatch",
    BranchifyMux => "branchify_mux",
    SplitCoalescedStores => "split_coalesced_stores",
    PartialForward => "partial_forward",
    IdentityStoreBypass => "identity_store_bypass",
    /// Compatibility selector for Cranelift oversized-function planning.
    /// This is not a SIR transform and is consumed at the backend boundary.
    TailCallSplit => "tail_call_split",
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
    use std::collections::HashSet;

    #[test]
    fn every_pass_name_round_trips_and_is_unique() {
        let names = SirPass::ALL
            .iter()
            .map(|&pass| {
                let name = pass.as_str();
                assert_eq!(SirPass::parse(name), Some(pass));
                name
            })
            .collect::<HashSet<_>>();

        assert_eq!(names.len(), SirPass::ALL.len());
    }

    #[test]
    fn o0_only_enables_the_backend_compatibility_selector() {
        let options = OptimizeOptions::new(OptLevel::O0);
        for &pass in SirPass::ALL {
            assert_eq!(options.is_enabled(pass), pass == SirPass::TailCallSplit);
        }
        assert!(!options.any_enabled());
    }

    #[test]
    fn explicit_overrides_take_precedence_over_the_level() {
        let options = OptimizeOptions::new(OptLevel::O0)
            .enable(SirPass::Gvn)
            .disable(SirPass::TailCallSplit);
        assert!(options.is_enabled(SirPass::Gvn));
        assert!(!options.is_enabled(SirPass::TailCallSplit));
        assert!(options.any_enabled());
    }

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
