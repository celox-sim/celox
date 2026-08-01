//! Cranelift-owned compilation policy.

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

    /// Select backend defaults corresponding to the facade optimization preset.
    pub fn for_opt_level(level: celox_sir_opt::OptLevel) -> Self {
        match level {
            celox_sir_opt::OptLevel::O0 => Self::fast_compile(),
            celox_sir_opt::OptLevel::O1 | celox_sir_opt::OptLevel::O2 => Self::default(),
        }
    }
}
