//! The cache of compiled designs.

use super::*;

// ---------------------------------------------------------------------------
//  Process-global compiled-code cache (native only)
// ---------------------------------------------------------------------------

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub(super) type SharedCode = celox::SharedNativeCode;

#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
pub(super) type SharedCode = celox::SharedJitCode;

#[cfg(not(target_arch = "wasm32"))]
/// Cached compilation result shared across simulator instances.
pub(super) struct CachedBuild {
    pub(super) shared_code: Arc<SharedCode>,
    pub(super) runtime_errors: HashMap<i64, (String, Vec<String>)>,
    pub(super) layout_json: String,
    pub(super) events_json: String,
    pub(super) hierarchy_json: String,
    pub(super) warnings_json: String,
    pub(super) stable_size: u32,
    pub(super) total_size: u32,
    pub(super) state_fingerprint: u64,
    pub(super) state_schema: Arc<celox::StateSchema>,
    /// Pre-computed VCD signal descriptors so VCD works on cache hits.
    pub(super) vcd_descs: Vec<celox::VcdSignalDesc>,
    pub(super) time_precision: celox_design::TimePrecision,
    /// Derived clock domains per event, valid for every backend sharing
    /// `shared_code`.
    pub(super) derived_ticks: celox::DerivedTickPlans<DefaultEvent>,
    pub(super) event_edges: celox::EventEdges,
}

#[cfg(not(target_arch = "wasm32"))]
/// Exact cache key — no hashing, no collisions.
///
/// Contains the full source content + paths + top module + all compilation-
/// affecting options. Two builds produce the same `CacheKey` iff they would
/// produce identical compiled code.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct CacheKey {
    pub(super) diagnostic_policy: celox::DiagnosticPolicy,
    /// (path, content) sorted by path for determinism.
    pub(super) sources: Vec<(String, String)>,
    pub(super) top: String,
    pub(super) four_state: bool,
    pub(super) vcd_tracking: bool,
    pub(super) sir_optimization: SirOptimizationCacheKey,
    pub(super) cranelift_opt_level: u8,
    pub(super) regalloc_algorithm: u8,
    pub(super) enable_alias_analysis: bool,
    pub(super) enable_verifier: bool,
    pub(super) dead_store_policy: u8,
    pub(super) clock_type: Option<u8>,
    pub(super) reset_type: Option<u8>,
    pub(super) parameters: Vec<(String, u64)>,
    pub(super) false_loops: Vec<(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )>,
    pub(super) true_loops: Vec<(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )>,
    /// Effective clock/reset from metadata (from_project path).
    /// None when using the `new` constructor (no metadata).
    pub(super) metadata_clock_type: Option<u8>,
    pub(super) metadata_reset_type: Option<u8>,
}

/// Collision-free representation of every SIR code-generation option.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct SirOptimizationCacheKey {
    pub(super) opt_level: celox::OptLevel,
    pub(super) enabled_passes: Box<[bool]>,
    pub(super) max_native_memory_width: usize,
    pub(super) parallel_lanes: u32,
    pub(super) always_partition: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl From<&celox::OptimizeOptions> for SirOptimizationCacheKey {
    fn from(options: &celox::OptimizeOptions) -> Self {
        Self {
            opt_level: options.opt_level(),
            enabled_passes: celox::SirPass::ALL
                .iter()
                .map(|&pass| options.is_enabled(pass))
                .collect(),
            max_native_memory_width: options.max_native_memory_width(),
            parallel_lanes: options.parallel_lanes(),
            always_partition: options.parallel_partition() == celox::ParallelPartition::Always,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: the process-global JIT cache is shared by every JS thread (main and Node worker_threads) that loads the addon in one process"
)]
pub(super) static JIT_CACHE: std::sync::LazyLock<Mutex<HashMap<CacheKey, Arc<CachedBuild>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::default()));

#[cfg(not(target_arch = "wasm32"))]
/// Build a collision-free cache key from source content, top module, and options.
///
/// When `metadata` is `Some`, the effective clock/reset settings from
/// `Veryl.toml` are included in the key so that changing project config
/// invalidates the cache.
pub(super) fn build_cache_key(
    sources: &[(String, std::path::PathBuf)],
    top: &str,
    opts: &ParsedOptions,
    metadata: Option<&Metadata>,
) -> CacheKey {
    let mut sorted_sources: Vec<(String, String)> = sources
        .iter()
        .map(|(content, path)| (path.to_string_lossy().into_owned(), content.clone()))
        .collect();
    sorted_sources.sort_by(|a, b| a.0.cmp(&b.0));

    CacheKey {
        diagnostic_policy: opts
            .diagnostic_policy
            .clone()
            .with_inferred_latches(opts.allow_inferred_latches),
        sources: sorted_sources,
        top: top.to_string(),
        four_state: opts.four_state,
        vcd_tracking: opts.waveform.is_some(),
        sir_optimization: SirOptimizationCacheKey::from(&opts.optimize_options),
        cranelift_opt_level: opts.cranelift_options.opt_level as u8,
        regalloc_algorithm: opts.cranelift_options.regalloc_algorithm as u8,
        enable_alias_analysis: opts.cranelift_options.enable_alias_analysis,
        enable_verifier: opts.cranelift_options.enable_verifier,
        dead_store_policy: opts.dead_store_policy as u8,
        clock_type: opts.clock_type.map(|ct| ct as u8),
        reset_type: opts.reset_type.map(|rt| rt as u8),
        parameters: opts.parameters.clone(),
        false_loops: opts.false_loops.clone(),
        true_loops: opts.true_loops.clone(),
        metadata_clock_type: metadata.map(|m| m.build.clock_type as u8),
        metadata_reset_type: metadata.map(|m| m.build.reset_type as u8),
    }
}
