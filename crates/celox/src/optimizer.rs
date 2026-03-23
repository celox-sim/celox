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

/// Per-pass enable/disable flags for fine-grained optimizer control.
///
/// All passes default to `true` (enabled). Set individual fields to `false`
/// to skip specific optimization passes while keeping others active.
#[derive(Debug, Clone, Copy)]
pub struct OptimizeOptions {
    pub store_load_forwarding: bool,
    pub hoist_common_branch_loads: bool,
    pub bit_extract_peephole: bool,
    pub optimize_blocks: bool,
    pub split_wide_commits: bool,
    pub commit_sinking: bool,
    pub inline_commit_forwarding: bool,
    pub eliminate_dead_working_stores: bool,
    pub reschedule: bool,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            store_load_forwarding: true,
            hoist_common_branch_loads: true,
            bit_extract_peephole: true,
            optimize_blocks: true,
            split_wide_commits: true,
            commit_sinking: true,
            inline_commit_forwarding: true,
            eliminate_dead_working_stores: true,
            reschedule: true,
        }
    }
}

impl OptimizeOptions {
    /// All passes enabled.
    pub fn all() -> Self {
        Self::default()
    }

    /// All passes disabled.
    pub fn none() -> Self {
        Self {
            store_load_forwarding: false,
            hoist_common_branch_loads: false,
            bit_extract_peephole: false,
            optimize_blocks: false,
            split_wide_commits: false,
            commit_sinking: false,
            inline_commit_forwarding: false,
            eliminate_dead_working_stores: false,
            reschedule: false,
        }
    }

    /// Returns true if any pass is enabled.
    pub fn any_enabled(&self) -> bool {
        self.store_load_forwarding
            || self.hoist_common_branch_loads
            || self.bit_extract_peephole
            || self.optimize_blocks
            || self.split_wide_commits
            || self.commit_sinking
            || self.inline_commit_forwarding
            || self.eliminate_dead_working_stores
            || self.reschedule
    }
}

#[derive(Debug, Clone, Copy)]
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

/// Run only the tail-call splitting pass (no other optimizations).
/// Used when `optimize=false` to still avoid exceeding Cranelift's instruction limit.
pub fn split_if_needed(program: &mut Program, four_state: bool) {
    use coalescing::pass_tail_call_split;

    // split_with_threshold / split_multi_block_with_threshold do their own
    // cost checks internally, so we delegate directly without a redundant
    // estimate_units_cost call here.
    if let Some(chunks) = pass_tail_call_split::split_if_needed(&program.eval_comb, four_state) {
        program.eval_comb_plan = Some(crate::ir::EvalCombPlan::TailCallChunks(chunks));
    } else if let Some(plan) =
        pass_tail_call_split::split_if_needed_spilled(&program.eval_comb, four_state)
    {
        program.eval_comb_plan = Some(crate::ir::EvalCombPlan::MemorySpilled(plan));
    }
}

pub fn optimize(program: &mut Program, four_state: bool, optimize_options: &OptimizeOptions) {
    let mut manager = PassManager::new();
    manager.add_pass(coalescing::CoalescingPass);
    manager.run(
        program,
        &PassOptions {
            four_state,
            optimize_options: *optimize_options,
            ..PassOptions::default()
        },
    );
}
