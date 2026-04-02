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
    /// No SIR optimizations except [`SirPass::TailCallSplit`].
    /// Cranelift: `fast_compile()`. DSE: Off.
    O0,
    /// All SIR optimizations enabled.
    /// Cranelift: Speed / Backtracking. DSE: Off.
    #[default]
    O1,
    /// All SIR optimizations + DSE(`PreserveTopPorts`).
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
    pub fn from_str(s: &str) -> Option<Self> {
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
    SplitCoalescedStores,
    PartialForward,
    IdentityStoreBypass,
    TailCallSplit,
}

impl SirPass {
    /// All pass variants in definition order.
    pub const ALL: &[SirPass] = &[
        SirPass::StoreLoadForwarding,
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
        SirPass::SplitCoalescedStores,
        SirPass::PartialForward,
        SirPass::IdentityStoreBypass,
        SirPass::TailCallSplit,
    ];

    /// Snake_case string representation (for NAPI/TS serialization).
    pub fn as_str(self) -> &'static str {
        match self {
            SirPass::StoreLoadForwarding => "store_load_forwarding",
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
            SirPass::SplitCoalescedStores => "split_coalesced_stores",
            SirPass::PartialForward => "partial_forward",
            SirPass::IdentityStoreBypass => "identity_store_bypass",
            SirPass::TailCallSplit => "tail_call_split",
        }
    }

    /// Parse from snake_case string (for NAPI/CLI).
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "store_load_forwarding" => Some(SirPass::StoreLoadForwarding),
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
/// // All passes enabled (default)
/// let opts = OptimizeOptions::default();
/// assert!(opts.is_enabled(SirPass::Gvn));
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
        }
    }

    /// All passes enabled (equivalent to `OptLevel::O1`).
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
}

impl Default for PassOptions {
    fn default() -> Self {
        Self {
            max_inflight_loads: 8,
            four_state: false,
            optimize_options: OptimizeOptions::default(),
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
    let mut manager = PassManager::new();
    manager.add_pass(coalescing::CoalescingPass);
    manager.run(
        program,
        &PassOptions {
            four_state,
            optimize_options: optimize_options.clone(),
            ..PassOptions::default()
        },
    );
}
