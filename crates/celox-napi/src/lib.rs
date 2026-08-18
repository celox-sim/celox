mod layout;

use fxhash::FxHashMap as HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::*;
use napi_derive::napi;
use veryl_analyzer::{Analyzer, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::Parser;
use veryl_path::PathSet;

#[cfg(target_arch = "x86_64")]
use celox::SimBackend;
#[cfg(not(target_arch = "wasm32"))]
use layout::{build_event_map, build_hierarchy_node, build_signal_layout};

/// A segment of a hierarchical instance path.
#[napi(object)]
pub struct NapiInstanceSegment {
    pub name: String,
    pub index: u32,
}

/// A signal path consisting of an instance path and a variable path.
#[napi(object)]
pub struct NapiSignalPath {
    pub instance_path: Vec<NapiInstanceSegment>,
    pub var_path: Vec<String>,
}

/// A false-loop declaration (combinational loop to ignore).
#[napi(object)]
pub struct NapiFalseLoop {
    pub from: NapiSignalPath,
    pub to: NapiSignalPath,
}

/// A true-loop declaration with a convergence iteration limit.
#[napi(object)]
pub struct NapiTrueLoop {
    pub from: NapiSignalPath,
    pub to: NapiSignalPath,
    pub max_iter: u32,
}

/// A source file with its content and path.
#[napi(object)]
pub struct NapiSourceFile {
    pub content: String,
    pub path: String,
}

/// A parameter override for a top-level module parameter.
#[napi(object)]
pub struct NapiParamOverride {
    pub name: String,
    pub value: i64,
}

/// Per-pass optimizer control. All fields default to true when omitted.
#[napi(object)]
pub struct NapiOptimizeOptions {
    pub store_load_forwarding: Option<bool>,
    pub hoist_common_branch_loads: Option<bool>,
    pub bit_extract_peephole: Option<bool>,
    pub optimize_blocks: Option<bool>,
    pub split_wide_commits: Option<bool>,
    pub commit_sinking: Option<bool>,
    pub inline_commit_forwarding: Option<bool>,
    pub eliminate_dead_working_stores: Option<bool>,
    pub reschedule: Option<bool>,
    pub coalesce_stores: Option<bool>,
}

/// Options for creating a simulator/simulation handle.
#[napi(object)]
pub struct NapiOptions {
    pub four_state: Option<bool>,
    pub vcd: Option<String>,
    /// Optimization level preset: "O0", "O1", or "O2".
    /// Takes precedence over `optimize` and `optimize_options`.
    pub opt_level: Option<String>,
    /// Per-pass overrides applied on top of opt_level.
    /// Each entry: "+sir:<pass_name>" to enable, "-sir:<pass_name>" to disable.
    pub pass_overrides: Option<Vec<String>>,
    /// Shorthand to enable/disable all SIRT optimization passes.
    /// `true` = all on, `false` = all off. Overridden by `opt_level` or `optimize_options`.
    pub optimize: Option<bool>,
    /// Per-pass optimizer flags (legacy). Overridden by `opt_level`/`pass_overrides`.
    pub optimize_options: Option<NapiOptimizeOptions>,
    /// Cranelift backend optimization level: "none", "speed", or "speed_and_size".
    pub cranelift_opt_level: Option<String>,
    /// Register allocator algorithm: "backtracking" or "single_pass".
    pub regalloc_algorithm: Option<String>,
    /// Enable alias analysis in the Cranelift egraph pass. Default: true.
    pub enable_alias_analysis: Option<bool>,
    /// Enable the Cranelift IR verifier. Default: true.
    pub enable_verifier: Option<bool>,
    pub false_loops: Option<Vec<NapiFalseLoop>>,
    pub true_loops: Option<Vec<NapiTrueLoop>>,
    /// Clock polarity: "posedge" or "negedge".
    pub clock_type: Option<String>,
    /// Reset type: "async_high", "async_low", "sync_high", or "sync_low".
    pub reset_type: Option<String>,
    /// Additional Veryl source to append to the main source code.
    pub extra_source: Option<String>,
    /// Parameter overrides for the top-level module.
    pub parameters: Option<Vec<NapiParamOverride>>,
    /// Dead store elimination policy: "off", "preserve_top_ports", or "preserve_all_ports".
    pub dead_store_policy: Option<String>,
}

/// Parsed builder options from NapiOptions (common fields available on all targets).
#[allow(dead_code)]
struct ParsedOptionsCommon {
    four_state: bool,
    optimize_options: celox::OptimizeOptions,
    vcd: Option<String>,
    false_loops: Vec<(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )>,
    true_loops: Vec<(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )>,
    clock_type: Option<celox::ClockType>,
    reset_type: Option<celox::ResetType>,
    extra_source: Option<String>,
    parameters: Vec<(String, u64)>,
}

/// Parsed builder options from NapiOptions (native-only, includes Cranelift/DSE options).
#[cfg(not(target_arch = "wasm32"))]
struct ParsedOptions {
    common: ParsedOptionsCommon,
    cranelift_options: celox::CraneliftOptions,
    dead_store_policy: celox::DeadStorePolicy,
}

#[cfg(not(target_arch = "wasm32"))]
impl std::ops::Deref for ParsedOptions {
    type Target = ParsedOptionsCommon;
    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

/// Convert a NapiSignalPath to the Rust builder's tuple format.
fn convert_signal_path(p: &NapiSignalPath) -> (Vec<(String, usize)>, Vec<String>) {
    let inst: Vec<(String, usize)> = p
        .instance_path
        .iter()
        .map(|seg| (seg.name.clone(), seg.index as usize))
        .collect();
    let var_path: Vec<String> = p.var_path.clone();
    (inst, var_path)
}

/// Parse a clock type string into ClockType.
fn parse_clock_type(s: &str) -> Result<celox::ClockType> {
    match s {
        "posedge" => Ok(celox::ClockType::PosEdge),
        "negedge" => Ok(celox::ClockType::NegEdge),
        _ => Err(Error::from_reason(format!(
            "Invalid clock_type '{}'. Expected 'posedge' or 'negedge'.",
            s
        ))),
    }
}

/// Parse a reset type string into ResetType.
fn parse_reset_type(s: &str) -> Result<celox::ResetType> {
    match s {
        "async_high" => Ok(celox::ResetType::AsyncHigh),
        "async_low" => Ok(celox::ResetType::AsyncLow),
        "sync_high" => Ok(celox::ResetType::SyncHigh),
        "sync_low" => Ok(celox::ResetType::SyncLow),
        _ => Err(Error::from_reason(format!(
            "Invalid reset_type '{}'. Expected 'async_high', 'async_low', 'sync_high', or 'sync_low'.",
            s
        ))),
    }
}

/// Convert legacy NapiOptimizeOptions to celox::OptimizeOptions.
/// Starts from O1 (all on) and disables any explicitly false fields.
fn convert_optimize_options(napi: &NapiOptimizeOptions) -> celox::OptimizeOptions {
    let mut opts = celox::OptimizeOptions::all();
    let fields: &[(Option<bool>, celox::SirPass)] = &[
        (
            napi.store_load_forwarding,
            celox::SirPass::StoreLoadForwarding,
        ),
        (
            napi.hoist_common_branch_loads,
            celox::SirPass::HoistCommonBranchLoads,
        ),
        (
            napi.bit_extract_peephole,
            celox::SirPass::BitExtractPeephole,
        ),
        (napi.optimize_blocks, celox::SirPass::OptimizeBlocks),
        (napi.split_wide_commits, celox::SirPass::SplitWideCommits),
        (napi.commit_sinking, celox::SirPass::CommitSinking),
        (
            napi.inline_commit_forwarding,
            celox::SirPass::InlineCommitForwarding,
        ),
        (
            napi.eliminate_dead_working_stores,
            celox::SirPass::EliminateDeadWorkingStores,
        ),
        (napi.reschedule, celox::SirPass::Reschedule),
        (napi.coalesce_stores, celox::SirPass::CoalesceStores),
    ];
    for &(val, pass) in fields {
        if let Some(false) = val {
            opts = opts.disable(pass);
        }
    }
    opts
}

/// Parse pass override strings like "+sir:reschedule" or "-sir:coalesce_stores".
fn apply_pass_overrides(
    mut opts: celox::OptimizeOptions,
    overrides: &[String],
) -> Result<celox::OptimizeOptions> {
    for s in overrides {
        let (enable, rest) = if let Some(rest) = s.strip_prefix('+') {
            (true, rest)
        } else if let Some(rest) = s.strip_prefix('-') {
            (false, rest)
        } else {
            return Err(Error::from_reason(format!(
                "Invalid pass override '{}'. Must start with '+' or '-'.",
                s
            )));
        };
        let pass_name = rest.strip_prefix("sir:").unwrap_or(rest);
        let pass = celox::SirPass::parse(pass_name).ok_or_else(|| {
            Error::from_reason(format!(
                "Unknown SIR pass '{}'. Valid passes: {}",
                pass_name,
                celox::SirPass::ALL
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        opts = if enable {
            opts.enable(pass)
        } else {
            opts.disable(pass)
        };
    }
    Ok(opts)
}

/// Parse a Cranelift optimization level string.
#[cfg(not(target_arch = "wasm32"))]
fn parse_cranelift_opt_level(s: &str) -> Result<celox::CraneliftOptLevel> {
    match s {
        "none" => Ok(celox::CraneliftOptLevel::None),
        "speed" => Ok(celox::CraneliftOptLevel::Speed),
        "speed_and_size" => Ok(celox::CraneliftOptLevel::SpeedAndSize),
        _ => Err(Error::from_reason(format!(
            "Invalid cranelift_opt_level '{}'. Expected 'none', 'speed', or 'speed_and_size'.",
            s
        ))),
    }
}

/// Parse a register allocator algorithm string.
#[cfg(not(target_arch = "wasm32"))]
fn parse_regalloc_algorithm(s: &str) -> Result<celox::RegallocAlgorithm> {
    match s {
        "backtracking" => Ok(celox::RegallocAlgorithm::Backtracking),
        "single_pass" => Ok(celox::RegallocAlgorithm::SinglePass),
        _ => Err(Error::from_reason(format!(
            "Invalid regalloc_algorithm '{}'. Expected 'backtracking' or 'single_pass'.",
            s
        ))),
    }
}

/// Parse a dead store policy string into DeadStorePolicy.
#[cfg(not(target_arch = "wasm32"))]
fn parse_dead_store_policy(s: &str) -> Result<celox::DeadStorePolicy> {
    match s {
        "off" => Ok(celox::DeadStorePolicy::Off),
        "preserve_top_ports" => Ok(celox::DeadStorePolicy::PreserveTopPorts),
        "preserve_all_ports" => Ok(celox::DeadStorePolicy::PreserveAllPorts),
        _ => Err(Error::from_reason(format!(
            "Invalid dead_store_policy '{}'. Expected 'off', 'preserve_top_ports', or 'preserve_all_ports'.",
            s
        ))),
    }
}

/// Helper to extract the common builder config from NapiOptions (available on all targets).
fn parse_options_common(options: &Option<NapiOptions>) -> Result<ParsedOptionsCommon> {
    match options.as_ref() {
        Some(o) => {
            let false_loops = o
                .false_loops
                .as_ref()
                .map(|loops| {
                    loops
                        .iter()
                        .map(|fl| (convert_signal_path(&fl.from), convert_signal_path(&fl.to)))
                        .collect()
                })
                .unwrap_or_default();
            let true_loops = o
                .true_loops
                .as_ref()
                .map(|loops| {
                    loops
                        .iter()
                        .map(|tl| {
                            (
                                convert_signal_path(&tl.from),
                                convert_signal_path(&tl.to),
                                tl.max_iter as usize,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let clock_type = o.clock_type.as_deref().map(parse_clock_type).transpose()?;
            let reset_type = o.reset_type.as_deref().map(parse_reset_type).transpose()?;
            let parameters = o
                .parameters
                .as_ref()
                .map(|params| {
                    params
                        .iter()
                        .map(|p| (p.name.clone(), p.value as u64))
                        .collect()
                })
                .unwrap_or_default();
            // Resolve optimize_options with priority:
            // 1. opt_level + pass_overrides (new API)
            // 2. optimize_options (legacy per-pass bools)
            // 3. optimize shorthand (legacy bool)
            // 4. default (O1 = all on)
            let optimize_options = if let Some(ref level_str) = o.opt_level {
                let level = celox::OptLevel::parse(level_str).ok_or_else(|| {
                    Error::from_reason(format!(
                        "Invalid opt_level '{}'. Expected 'O0', 'O1', or 'O2'.",
                        level_str
                    ))
                })?;
                let opts = celox::OptimizeOptions::new(level);
                if let Some(ref overrides) = o.pass_overrides {
                    apply_pass_overrides(opts, overrides)?
                } else {
                    opts
                }
            } else if let Some(ref oo) = o.optimize_options {
                let opts = convert_optimize_options(oo);
                if let Some(ref overrides) = o.pass_overrides {
                    apply_pass_overrides(opts, overrides)?
                } else {
                    opts
                }
            } else if let Some(false) = o.optimize {
                celox::OptimizeOptions::none()
            } else {
                celox::OptimizeOptions::all()
            };
            Ok(ParsedOptionsCommon {
                four_state: o.four_state.unwrap_or(false),
                optimize_options,
                vcd: o.vcd.clone(),
                false_loops,
                true_loops,
                clock_type,
                reset_type,
                extra_source: o.extra_source.clone(),
                parameters,
            })
        }
        None => Ok(ParsedOptionsCommon {
            four_state: false,
            optimize_options: celox::OptimizeOptions::all(),
            vcd: None,
            false_loops: Vec::new(),
            true_loops: Vec::new(),
            clock_type: None,
            reset_type: None,
            extra_source: None,
            parameters: Vec::new(),
        }),
    }
}

/// Helper to extract the full builder config from NapiOptions (native only).
#[cfg(not(target_arch = "wasm32"))]
fn parse_options(options: &Option<NapiOptions>) -> Result<ParsedOptions> {
    let common = parse_options_common(options)?;
    match options.as_ref() {
        Some(o) => {
            let dead_store_policy = o
                .dead_store_policy
                .as_deref()
                .map(parse_dead_store_policy)
                .transpose()?
                .unwrap_or(celox::DeadStorePolicy::Off);
            let cranelift_opt_level = o
                .cranelift_opt_level
                .as_deref()
                .map(parse_cranelift_opt_level)
                .transpose()?
                .unwrap_or(celox::CraneliftOptLevel::Speed);
            let regalloc_algorithm = o
                .regalloc_algorithm
                .as_deref()
                .map(parse_regalloc_algorithm)
                .transpose()?
                .unwrap_or(celox::RegallocAlgorithm::Backtracking);
            let cranelift_options = celox::CraneliftOptions {
                opt_level: cranelift_opt_level,
                regalloc_algorithm,
                enable_alias_analysis: o.enable_alias_analysis.unwrap_or(true),
                enable_verifier: o.enable_verifier.unwrap_or(true),
                tail_call_split: true,
                diagnostics: celox::CraneliftDiagnostics::default(),
            };
            Ok(ParsedOptions {
                common,
                cranelift_options,
                dead_store_policy,
            })
        }
        None => Ok(ParsedOptions {
            common,
            cranelift_options: celox::CraneliftOptions::default(),
            dead_store_policy: celox::DeadStorePolicy::Off,
        }),
    }
}

/// Append extra source as a separate file entry if provided.
fn append_extra_source(sources: &mut Vec<(String, std::path::PathBuf)>, extra: &Option<String>) {
    if let Some(extra) = extra {
        sources.push((extra.clone(), std::path::PathBuf::from("<extra>")));
    }
}

/// Configuration loaded from an optional `celox.toml` in the project root.
#[derive(serde::Deserialize, Default)]
#[allow(dead_code)]
struct CeloxConfig {
    /// Glob patterns (relative to project root) for `.veryl` files to exclude
    /// from compilation and type generation.
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    test: CeloxTestConfig,
    #[serde(default)]
    simulation: CeloxSimulationConfig,
}

#[derive(serde::Deserialize, Default)]
struct CeloxTestConfig {
    /// Additional source directories (relative to `celox.toml`) whose `.veryl`
    /// files are included when running simulations and generating type stubs.
    #[serde(default)]
    sources: Vec<String>,
}

#[derive(serde::Deserialize, Default)]
#[allow(dead_code)]
struct CeloxSimulationConfig {
    /// Default maximum steps for `waitUntil` / `waitForCycles`.
    /// Overridden by the per-call `maxSteps` option.
    max_steps: Option<u32>,
}

/// Load `celox.toml` from the given project root (same directory as `Veryl.toml`).
/// Returns `None` if the file does not exist.
fn load_celox_config(project_root: &std::path::Path) -> Result<CeloxConfig> {
    let path = project_root.join("celox.toml");
    if !path.exists() {
        return Ok(CeloxConfig::default());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| Error::from_reason(format!("Failed to read celox.toml: {e}")))?;
    toml::from_str(&content)
        .map_err(|e| Error::from_reason(format!("Failed to parse celox.toml: {e}")))
}

/// Build a `GlobSet` from the exclude patterns in the config.
/// Returns `None` if there are no exclude patterns.
fn build_exclude_set(config: &CeloxConfig) -> Result<Option<globset::GlobSet>> {
    if config.exclude.is_empty() {
        return Ok(None);
    }
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in &config.exclude {
        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|e| Error::from_reason(format!("Invalid exclude pattern '{pattern}': {e}")))?;
        builder.add(glob);
    }
    let set = builder
        .build()
        .map_err(|e| Error::from_reason(format!("Failed to build exclude set: {e}")))?;
    Ok(Some(set))
}

/// Returns `true` if the path should be excluded based on the glob set.
/// The path is matched relative to `project_root`.
fn is_excluded(
    path: &std::path::Path,
    project_root: &std::path::Path,
    exclude_set: &globset::GlobSet,
) -> bool {
    let relative = path.strip_prefix(project_root).unwrap_or(path);
    // Normalize to forward slashes for consistent matching
    let rel_str = relative.to_string_lossy().replace('\\', "/");
    exclude_set.is_match(&rel_str)
}

/// Collect all `.veryl` files from the extra test source directories declared in
/// `celox.toml` and add them as individual source entries.
fn collect_test_sources(
    sources: &mut Vec<(String, std::path::PathBuf)>,
    project_root: &std::path::Path,
    config: &CeloxConfig,
) -> Result<()> {
    for dir in &config.test.sources {
        let dir_path = project_root.join(dir);
        if !dir_path.exists() {
            continue;
        }
        let entries = walkdir(&dir_path)?;
        for entry in entries {
            let content = std::fs::read_to_string(&entry)
                .map_err(|e| Error::from_reason(format!("{}: {e}", entry.display())))?;
            sources.push((content, entry));
        }
    }
    Ok(())
}

/// Recursively collect `.veryl` files under `dir`, sorted for determinism.
fn walkdir(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    let read = std::fs::read_dir(dir)
        .map_err(|e| Error::from_reason(format!("Cannot read directory {}: {e}", dir.display())))?;
    for entry in read {
        let entry = entry.map_err(|e| Error::from_reason(format!("Directory entry error: {e}")))?;
        let path = entry.path();
        if path.is_dir() {
            files.extend(walkdir(&path)?);
        } else if path.extension().is_some_and(|ext| ext == "veryl") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Load a Veryl project's source files and metadata from a directory.
///
/// Searches upward from `project_path` for `Veryl.toml`, gathers all `.veryl`
/// source files, and returns the per-file sources, project metadata, and
/// the parsed `celox.toml` configuration.
fn load_project_sources(
    project_path: &str,
) -> Result<(Vec<(String, std::path::PathBuf)>, Metadata, CeloxConfig)> {
    let toml_path = Metadata::search_from(project_path)
        .map_err(|e| Error::from_reason(format!("Could not find Veryl.toml: {e}")))?;
    let mut metadata = Metadata::load(&toml_path)
        .map_err(|e| Error::from_reason(format!("Failed to load Veryl.toml: {e}")))?;
    let paths = metadata
        .paths::<&str>(&[], false, false)
        .map_err(|e| Error::from_reason(format!("Failed to gather sources: {e}")))?;
    let mut sources = Vec::new();
    for p in paths.iter().filter(|path| !path.example) {
        let content = std::fs::read_to_string(&p.src)
            .map_err(|e| Error::from_reason(format!("{}: {e}", p.src.display())))?;
        sources.push((content, p.src.clone()));
    }
    let project_root = toml_path.parent().unwrap_or(&toml_path);
    let celox_cfg = load_celox_config(project_root)?;
    collect_test_sources(&mut sources, project_root, &celox_cfg)?;
    if let Some(exclude_set) = build_exclude_set(&celox_cfg)? {
        sources.retain(|(_, path)| !is_excluded(path, project_root, &exclude_set));
    }
    Ok((sources, metadata, celox_cfg))
}

/// Format compilation warnings as a JSON array of strings.
///
/// Uses `render_diagnostic` to include source location and span information,
/// matching the format used for error messages.
fn format_warnings_json(warnings: &[celox::CompilationWarning]) -> String {
    let msgs: Vec<String> = warnings
        .iter()
        .map(|w| celox::render_diagnostic(w))
        .collect();
    serde_json::to_string(&msgs).unwrap_or_else(|_| "[]".to_string())
}

/// Apply parsed options to a SimulatorBuilder.
#[cfg(not(target_arch = "wasm32"))]
fn apply_options<'a, T>(
    mut builder: celox::SimulatorBuilder<'a, T>,
    opts: &ParsedOptions,
) -> celox::SimulatorBuilder<'a, T> {
    builder = builder.four_state(opts.four_state);
    builder = builder.optimize_options(opts.optimize_options.clone());
    builder = builder.cranelift_options(opts.cranelift_options);
    // VCD is handled separately after build — not passed to SimulatorBuilder
    for (from, to) in &opts.false_loops {
        builder = builder.false_loop(from.clone(), to.clone());
    }
    for (from, to, max_iter) in &opts.true_loops {
        builder = builder.true_loop(from.clone(), to.clone(), *max_iter);
    }
    if let Some(ct) = opts.clock_type {
        builder = builder.clock_type(ct);
    }
    if let Some(rt) = opts.reset_type {
        builder = builder.reset_type(rt);
    }
    for (name, value) in &opts.parameters {
        builder = builder.param(name, *value);
    }
    builder = builder.dead_store_policy(opts.dead_store_policy);
    builder
}

// ---------------------------------------------------------------------------
//  Process-global JIT cache (native only)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
type SharedCode = celox::SharedNativeCode;
#[cfg(all(not(target_arch = "wasm32"), not(target_arch = "x86_64")))]
type SharedCode = celox::SharedJitCode;

#[cfg(not(target_arch = "wasm32"))]
/// Cached compilation result shared across simulator instances.
struct CachedBuild {
    shared_code: Arc<SharedCode>,
    runtime_errors: HashMap<i64, (String, Vec<String>)>,
    layout_json: String,
    events_json: String,
    hierarchy_json: String,
    warnings_json: String,
    stable_size: u32,
    total_size: u32,
    /// Pre-computed VCD signal descriptors so VCD works on cache hits.
    vcd_descs: Vec<celox::VcdSignalDesc>,
}

#[cfg(not(target_arch = "wasm32"))]
/// Exact cache key — no hashing, no collisions.
///
/// Contains the full source content + paths + top module + all compilation-
/// affecting options. Two builds produce the same `CacheKey` iff they would
/// produce identical compiled code.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    /// (path, content) sorted by path for determinism.
    sources: Vec<(String, String)>,
    top: String,
    four_state: bool,
    sir_optimization: SirOptimizationCacheKey,
    cranelift_opt_level: u8,
    regalloc_algorithm: u8,
    enable_alias_analysis: bool,
    enable_verifier: bool,
    dead_store_policy: u8,
    clock_type: Option<u8>,
    reset_type: Option<u8>,
    parameters: Vec<(String, u64)>,
    false_loops: Vec<(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )>,
    true_loops: Vec<(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )>,
    /// Effective clock/reset from metadata (from_project path).
    /// None when using the `new` constructor (no metadata).
    metadata_clock_type: Option<u8>,
    metadata_reset_type: Option<u8>,
}

/// Collision-free representation of every SIR code-generation option.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SirOptimizationCacheKey {
    opt_level: celox::OptLevel,
    enabled_passes: Box<[bool]>,
    max_native_memory_width: usize,
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
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
static JIT_CACHE: std::sync::LazyLock<Mutex<HashMap<CacheKey, Arc<CachedBuild>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::default()));

#[cfg(not(target_arch = "wasm32"))]
/// Build a collision-free cache key from source content, top module, and options.
///
/// When `metadata` is `Some`, the effective clock/reset settings from
/// `Veryl.toml` are included in the key so that changing project config
/// invalidates the cache.
fn build_cache_key(
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
        sources: sorted_sources,
        top: top.to_string(),
        four_state: opts.four_state,
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

#[cfg(not(target_arch = "wasm32"))]
fn runtime_errors_by_name(program: &celox::RuntimeProgram) -> HashMap<i64, (String, Vec<String>)> {
    program
        .runtime_schema
        .runtime_errors
        .iter()
        .map(|(&code, info)| {
            (
                code,
                (
                    info.message.clone(),
                    info.signals
                        .iter()
                        .map(|addr| program.get_path(addr))
                        .collect(),
                ),
            )
        })
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
fn napi_runtime_error(
    runtime_errors: &HashMap<i64, (String, Vec<String>)>,
    err: celox::RuntimeErrorCode,
) -> Error {
    match err {
        celox::RuntimeErrorCode::DetectedTrueLoopCode(code) => {
            if let Some((message, signals)) = runtime_errors.get(&code) {
                if message == "Detected True Loop" {
                    Error::from_reason(format!(
                        "{}",
                        celox::RuntimeErrorCode::DetectedTrueLoopAt {
                            signals: signals.clone(),
                        }
                    ))
                } else {
                    Error::from_reason(format!(
                        "{}",
                        celox::RuntimeErrorCode::Runtime {
                            message: message.clone(),
                            signals: signals.clone(),
                        }
                    ))
                }
            } else {
                Error::from_reason(format!("{}", celox::RuntimeErrorCode::DetectedTrueLoop))
            }
        }
        other => Error::from_reason(format!("{}", other)),
    }
}

/// Low-level handle wrapping a JIT backend and optional VCD writer.
///
/// JS holds this as an opaque class; all operations go through methods.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub struct NativeSimulatorHandle {
    backend: Option<celox::DefaultBackend>,
    runtime_errors: HashMap<i64, (String, Vec<String>)>,
    vcd_writer: Option<celox::VcdWriter>,
    layout_json: String,
    events_json: String,
    hierarchy_json: String,
    warnings_json: String,
    stable_size: u32,
    total_size: u32,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi]
impl NativeSimulatorHandle {
    /// Build a full simulator, extract metadata, cache the compiled code,
    /// and return the handle with a JitBackend (and optional VcdWriter).
    fn build_and_cache(
        sim: celox::Simulator,
        four_state: bool,
        vcd_path: Option<&str>,
        cache_key: Option<CacheKey>,
    ) -> Result<Self> {
        let warnings_json = format_warnings_json(sim.warnings());
        let signals = sim.named_signals();
        let events = sim.named_events();
        let hierarchy = sim.named_hierarchy();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();
        let vcd_descs = sim.build_vcd_descs(four_state);
        let runtime_errors = runtime_errors_by_name(sim.program());

        let layout_map = build_signal_layout(&signals, four_state);
        let event_map = build_event_map(&events);
        let hierarchy_node = build_hierarchy_node(&hierarchy, four_state);

        let layout_json = serde_json::to_string(&layout_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize layout: {}", e)))?;
        let events_json = serde_json::to_string(&event_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize events: {}", e)))?;
        let hierarchy_json = serde_json::to_string(&hierarchy_node)
            .map_err(|e| Error::from_reason(format!("Failed to serialize hierarchy: {}", e)))?;

        // Cache the compiled code + metadata for future instances
        if let Some(key) = cache_key {
            let cached = Arc::new(CachedBuild {
                shared_code: sim.shared_code(),
                runtime_errors: runtime_errors.clone(),
                layout_json: layout_json.clone(),
                events_json: events_json.clone(),
                hierarchy_json: hierarchy_json.clone(),
                warnings_json: warnings_json.clone(),
                stable_size: stable_size as u32,
                total_size: total_size as u32,
                vcd_descs: vcd_descs.clone(),
            });
            let mut cache = JIT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            cache.insert(key, cached);
        }

        // Create VcdWriter if requested
        let vcd_writer = if let Some(path) = vcd_path {
            Some(
                celox::VcdWriter::new(path, &vcd_descs)
                    .map_err(|e| Error::from_reason(format!("Failed to create VCD: {}", e)))?,
            )
        } else {
            None
        };

        // Extract JitBackend from Simulator (drops runtime metadata which is no longer needed)
        let backend = sim.into_backend();

        Ok(Self {
            backend: Some(backend),
            runtime_errors,
            vcd_writer,
            layout_json,
            events_json,
            hierarchy_json,
            warnings_json,
            stable_size: stable_size as u32,
            total_size: total_size as u32,
        })
    }

    /// Create a handle from a cached build (shared compiled code + fresh memory).
    fn from_cached(cached: &CachedBuild, vcd_path: Option<&str>) -> Result<Self> {
        #[cfg(target_arch = "x86_64")]
        let backend = celox::NativeBackend::from_shared(Arc::clone(&cached.shared_code));
        #[cfg(not(target_arch = "x86_64"))]
        let backend = celox::JitBackend::from_shared(Arc::clone(&cached.shared_code));
        let vcd_writer = if let Some(path) = vcd_path {
            Some(
                celox::VcdWriter::new(path, &cached.vcd_descs)
                    .map_err(|e| Error::from_reason(format!("Failed to create VCD: {}", e)))?,
            )
        } else {
            None
        };
        Ok(Self {
            backend: Some(backend),
            runtime_errors: cached.runtime_errors.clone(),
            vcd_writer,
            layout_json: cached.layout_json.clone(),
            events_json: cached.events_json.clone(),
            hierarchy_json: cached.hierarchy_json.clone(),
            warnings_json: cached.warnings_json.clone(),
            stable_size: cached.stable_size,
            total_size: cached.total_size,
        })
    }

    /// Create a new simulator from Veryl source code.
    #[napi(constructor)]
    pub fn new(
        sources: Vec<NapiSourceFile>,
        top: String,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let opts = parse_options(&options)?;
        let mut src_pairs: Vec<(String, std::path::PathBuf)> = sources
            .into_iter()
            .map(|s| (s.content, std::path::PathBuf::from(s.path)))
            .collect();
        append_extra_source(&mut src_pairs, &opts.extra_source);

        let cache_key = build_cache_key(&src_pairs, &top, &opts, None);

        {
            let cache = JIT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cache.get(&cache_key) {
                return Self::from_cached(cached, opts.vcd.as_deref());
            }
        }

        let source_refs: Vec<(&str, &std::path::Path)> = src_pairs
            .iter()
            .map(|(s, p)| (s.as_str(), p.as_path()))
            .collect();
        let builder = apply_options(celox::Simulator::from_sources(source_refs, &top), &opts);
        let sim = builder
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        Self::build_and_cache(sim, opts.four_state, opts.vcd.as_deref(), Some(cache_key))
    }

    /// Create a new simulator from a Veryl project directory.
    ///
    /// Searches upward from `project_path` for `Veryl.toml`, gathers all
    /// `.veryl` source files, and builds the simulator using the project's
    /// clock/reset settings.
    #[napi(factory)]
    pub fn from_project(
        project_path: String,
        top: String,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let opts = parse_options(&options)?;
        let (mut sources, metadata, _celox_cfg) = load_project_sources(&project_path)?;
        append_extra_source(&mut sources, &opts.extra_source);

        let cache_key = build_cache_key(&sources, &top, &opts, Some(&metadata));

        {
            let cache = JIT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cache.get(&cache_key) {
                return Self::from_cached(cached, opts.vcd.as_deref());
            }
        }

        let source_refs: Vec<(&str, &std::path::Path)> = sources
            .iter()
            .map(|(s, p)| (s.as_str(), p.as_path()))
            .collect();

        let builder = apply_options(
            celox::Simulator::from_sources(source_refs, &top).with_metadata(metadata),
            &opts,
        );
        let sim = builder
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        Self::build_and_cache(sim, opts.four_state, opts.vcd.as_deref(), Some(cache_key))
    }

    /// Returns the signal layout as a JSON string.
    #[napi(getter)]
    pub fn layout_json(&self) -> String {
        self.layout_json.clone()
    }

    /// Returns the event map as a JSON string.
    #[napi(getter)]
    pub fn events_json(&self) -> String {
        self.events_json.clone()
    }

    /// Returns the instance hierarchy as a JSON string.
    #[napi(getter)]
    pub fn hierarchy_json(&self) -> String {
        self.hierarchy_json.clone()
    }

    /// Returns compilation warnings as a JSON array of strings.
    #[napi(getter)]
    pub fn warnings_json(&self) -> String {
        self.warnings_json.clone()
    }

    /// Returns the stable region size in bytes.
    #[napi(getter)]
    pub fn stable_size(&self) -> u32 {
        self.stable_size
    }

    /// Returns the total memory size in bytes.
    #[napi(getter)]
    pub fn total_size(&self) -> u32 {
        self.total_size
    }

    /// Trigger a clock/event by its numeric ID.
    #[napi]
    pub fn tick(&mut self, event_id: u32) -> Result<()> {
        let runtime_errors = self.runtime_errors.clone();
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        let event = b.id_to_event_slice()[event_id as usize];
        b.eval_comb()
            .map_err(|e| napi_runtime_error(&runtime_errors, e))?;
        b.eval_apply_ff_at(event)
            .map_err(|e| napi_runtime_error(&runtime_errors, e))?;
        b.eval_comb()
            .map_err(|e| napi_runtime_error(&runtime_errors, e))
    }

    /// Trigger a clock/event N times in a single NAPI call.
    #[napi]
    pub fn tick_n(&mut self, event_id: u32, count: u32) -> Result<()> {
        let runtime_errors = self.runtime_errors.clone();
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        let event = b.id_to_event_slice()[event_id as usize];
        for _ in 0..count {
            b.eval_comb()
                .map_err(|e| napi_runtime_error(&runtime_errors, e))?;
            b.eval_apply_ff_at(event)
                .map_err(|e| napi_runtime_error(&runtime_errors, e))?;
            b.eval_comb()
                .map_err(|e| napi_runtime_error(&runtime_errors, e))?;
        }
        Ok(())
    }

    /// Evaluate combinational logic.
    #[napi]
    pub fn eval_comb(&mut self) -> Result<()> {
        let runtime_errors = self.runtime_errors.clone();
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        b.eval_comb()
            .map_err(|e| napi_runtime_error(&runtime_errors, e))
    }

    /// Write VCD dump at the given timestamp.
    #[napi]
    pub fn dump(&mut self, timestamp: f64) -> Result<()> {
        let b = self
            .backend
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        if let Some(ref mut writer) = self.vcd_writer {
            let (ptr, size) = b.memory_as_ptr();
            let memory = unsafe { std::slice::from_raw_parts(ptr, size) };
            writer
                .dump(timestamp as u64, memory)
                .map_err(|e| Error::from_reason(format!("VCD write error: {}", e)))?;
        }
        Ok(())
    }

    /// Return the simulator's stable memory region as a zero-copy `Uint8Array`.
    /// JS can access `.buffer` to get the underlying `ArrayBuffer`.
    #[napi]
    pub fn shared_memory(&mut self) -> Result<Uint8Array> {
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        let (ptr, _) = b.memory_as_mut_ptr();
        let stable_size = b.stable_region_size();
        Ok(unsafe { Uint8Array::with_external_data(ptr, stable_size, |_, _| {}) })
    }

    /// Invalidate this handle (no-op on the Rust side; drop happens via GC).
    #[napi]
    pub fn dispose(&mut self) {
        self.backend = None;
        self.vcd_writer = None;
    }
}

/// Low-level handle wrapping a `celox::Simulation`.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub struct NativeSimulationHandle {
    sim: Option<celox::Simulation>,
    layout_json: String,
    events_json: String,
    hierarchy_json: String,
    warnings_json: String,
    stable_size: u32,
    total_size: u32,
    /// Default `maxSteps` for `waitUntil` / `waitForCycles`, sourced from
    /// `[simulation] max_steps` in `celox.toml`. `None` when not set.
    default_max_steps: Option<u32>,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi]
impl NativeSimulationHandle {
    /// Create a new timed simulation from Veryl source code.
    #[napi(constructor)]
    pub fn new(
        sources: Vec<NapiSourceFile>,
        top: String,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let opts = parse_options(&options)?;
        let mut src_pairs: Vec<(String, std::path::PathBuf)> = sources
            .into_iter()
            .map(|s| (s.content, std::path::PathBuf::from(s.path)))
            .collect();
        append_extra_source(&mut src_pairs, &opts.extra_source);
        let source_refs: Vec<(&str, &std::path::Path)> = src_pairs
            .iter()
            .map(|(s, p)| (s.as_str(), p.as_path()))
            .collect();
        let mut builder = apply_options(celox::Simulation::from_sources(source_refs, &top), &opts);
        if let Some(path) = &opts.vcd {
            builder = builder.vcd(path);
        }
        let sim = builder
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let warnings_json = format_warnings_json(sim.warnings());
        let signals = sim.named_signals();
        let events = sim.named_events();
        let hierarchy = sim.named_hierarchy();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();

        let layout_map = build_signal_layout(&signals, opts.four_state);
        let event_map = build_event_map(&events);
        let hierarchy_node = build_hierarchy_node(&hierarchy, opts.four_state);

        let layout_json = serde_json::to_string(&layout_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize layout: {}", e)))?;
        let events_json = serde_json::to_string(&event_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize events: {}", e)))?;
        let hierarchy_json = serde_json::to_string(&hierarchy_node)
            .map_err(|e| Error::from_reason(format!("Failed to serialize hierarchy: {}", e)))?;

        Ok(Self {
            sim: Some(sim),
            layout_json,
            events_json,
            hierarchy_json,
            warnings_json,
            stable_size: stable_size as u32,
            total_size: total_size as u32,
            default_max_steps: None,
        })
    }

    /// Create a new timed simulation from a Veryl project directory.
    #[napi(factory)]
    pub fn from_project(
        project_path: String,
        top: String,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let opts = parse_options(&options)?;
        let (mut sources, metadata, celox_cfg) = load_project_sources(&project_path)?;
        append_extra_source(&mut sources, &opts.extra_source);
        let source_refs: Vec<(&str, &std::path::Path)> = sources
            .iter()
            .map(|(s, p)| (s.as_str(), p.as_path()))
            .collect();

        let mut builder = apply_options(
            celox::Simulation::from_sources(source_refs, &top).with_metadata(metadata),
            &opts,
        );
        if let Some(path) = &opts.vcd {
            builder = builder.vcd(path);
        }
        let sim = builder
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let warnings_json = format_warnings_json(sim.warnings());
        let signals = sim.named_signals();
        let events = sim.named_events();
        let hierarchy = sim.named_hierarchy();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();

        let layout_map = build_signal_layout(&signals, opts.four_state);
        let event_map = build_event_map(&events);
        let hierarchy_node = build_hierarchy_node(&hierarchy, opts.four_state);

        let layout_json = serde_json::to_string(&layout_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize layout: {}", e)))?;
        let events_json = serde_json::to_string(&event_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize events: {}", e)))?;
        let hierarchy_json = serde_json::to_string(&hierarchy_node)
            .map_err(|e| Error::from_reason(format!("Failed to serialize hierarchy: {}", e)))?;

        Ok(Self {
            sim: Some(sim),
            layout_json,
            events_json,
            hierarchy_json,
            warnings_json,
            stable_size: stable_size as u32,
            total_size: total_size as u32,
            default_max_steps: celox_cfg.simulation.max_steps,
        })
    }

    /// Returns the signal layout as a JSON string.
    #[napi(getter)]
    pub fn layout_json(&self) -> String {
        self.layout_json.clone()
    }

    /// Returns the event map as a JSON string.
    #[napi(getter)]
    pub fn events_json(&self) -> String {
        self.events_json.clone()
    }

    /// Returns the instance hierarchy as a JSON string.
    #[napi(getter)]
    pub fn hierarchy_json(&self) -> String {
        self.hierarchy_json.clone()
    }

    /// Returns compilation warnings as a JSON array of strings.
    #[napi(getter)]
    pub fn warnings_json(&self) -> String {
        self.warnings_json.clone()
    }

    /// Returns the stable region size in bytes.
    #[napi(getter)]
    pub fn stable_size(&self) -> u32 {
        self.stable_size
    }

    /// Returns the total memory size in bytes.
    #[napi(getter)]
    pub fn total_size(&self) -> u32 {
        self.total_size
    }

    /// Returns the default `maxSteps` from `[simulation] max_steps` in `celox.toml`,
    /// or `null` if not configured.
    #[napi(getter)]
    pub fn default_max_steps(&self) -> Option<u32> {
        self.default_max_steps
    }

    /// Register a clock by event ID.
    #[napi]
    pub fn add_clock(&mut self, event_id: u32, period: f64, initial_delay: f64) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.add_clock_by_id(event_id, period as u64, initial_delay as u64);
        Ok(())
    }

    /// Schedule a one-shot event by event ID.
    #[napi]
    pub fn schedule(&mut self, event_id: u32, time: f64, value: f64) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.schedule_by_id(event_id, time as u64, value as u64)
            .map_err(|e| Error::from_reason(format!("{}", e)))
    }

    /// Advance simulation until `end_time`.
    #[napi]
    pub fn run_until(&mut self, end_time: f64) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.run_until(end_time as u64)
            .map_err(|e| Error::from_reason(format!("{}", e)))
    }

    /// Advance to the next event. Returns the new time, or null if no events.
    #[napi]
    pub fn step(&mut self) -> Result<Option<f64>> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.step()
            .map(|opt| opt.map(|t| t as f64))
            .map_err(|e| Error::from_reason(format!("{}", e)))
    }

    /// Returns the current simulation time.
    #[napi]
    pub fn time(&self) -> Result<f64> {
        let sim = self
            .sim
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        Ok(sim.time() as f64)
    }

    /// Returns the time of the next scheduled event, or null if none.
    #[napi]
    pub fn next_event_time(&self) -> Result<Option<f64>> {
        let sim = self
            .sim
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        Ok(sim.next_event_time().map(|t| t as f64))
    }

    /// Evaluate combinational logic.
    #[napi]
    pub fn eval_comb(&mut self) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.eval_comb()
            .map_err(|e| Error::from_reason(format!("{}", e)))
    }

    /// Write VCD dump at the given timestamp.
    #[napi]
    pub fn dump(&mut self, timestamp: f64) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.dump(timestamp as u64);
        Ok(())
    }

    /// Return the simulation's stable memory region as a zero-copy `Uint8Array`.
    /// JS can access `.buffer` to get the underlying `ArrayBuffer`.
    #[napi]
    pub fn shared_memory(&mut self) -> Result<Uint8Array> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        let (ptr, _) = sim.memory_as_mut_ptr();
        let stable_size = sim.stable_region_size();
        Ok(unsafe { Uint8Array::with_external_data(ptr, stable_size, |_, _| {}) })
    }

    /// Invalidate this handle.
    #[napi]
    pub fn dispose(&mut self) {
        self.sim = None;
    }
}

// ---------------------------------------------------------------------------
//  WASM32 NativeSimulatorHandle — compiles Veryl to WASM bytecode
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[napi]
pub struct NativeSimulatorHandle {
    program: celox::LaidOutProgram,
    four_state: bool,
    layout_json: String,
    events_json: String,
    hierarchy_json: String,
    warnings_json: String,
    stable_size: u32,
    total_size: u32,
}

#[cfg(target_arch = "wasm32")]
#[napi]
impl NativeSimulatorHandle {
    /// Compile Veryl source code and produce a WASM-oriented handle.
    ///
    /// Unlike the native (JIT) variant, this handle does NOT execute
    /// simulation directly. Instead it exposes `combWasmBytes()` and
    /// `eventWasmBytes(name)` for the TS runtime to instantiate in the
    /// browser via WebAssembly.
    #[napi(constructor)]
    pub fn new(
        sources: Vec<NapiSourceFile>,
        top: String,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let opts = parse_options_common(&options)?;
        let mut src_pairs: Vec<(String, std::path::PathBuf)> = sources
            .into_iter()
            .map(|s| (s.content, std::path::PathBuf::from(s.path)))
            .collect();
        append_extra_source(&mut src_pairs, &opts.extra_source);

        let source_refs: Vec<(&str, &std::path::Path)> = src_pairs
            .iter()
            .map(|(s, p)| (s.as_str(), p.as_path()))
            .collect();

        let trace_opts = celox::TraceOptions::default();
        let (program, warnings) = celox::compile_to_sir(
            &source_refs,
            &top,
            &opts
                .false_loops
                .iter()
                .map(|(f, t)| (f.clone(), t.clone()))
                .collect::<Vec<_>>(),
            &opts
                .true_loops
                .iter()
                .map(|(f, t, m)| (f.clone(), t.clone(), *m))
                .collect::<Vec<_>>(),
            opts.four_state,
            &trace_opts,
            None,
            None,
            opts.clock_type,
            opts.reset_type,
            &opts.parameters,
            &opts.optimize_options,
        )
        .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let laid_out = program.into_laid_out(opts.four_state);
        let layout = laid_out.layout();

        let layout_json = Self::build_layout_json(&laid_out, layout, opts.four_state);
        let events_json = Self::build_events_json(&laid_out);
        let hierarchy_json = "{}".to_string(); // Hierarchy not available on wasm32
        let warnings_json = format_warnings_json(&warnings);

        let stable_size = layout.total_size as u32;
        let total_size = layout.merged_total_size as u32;
        Ok(Self {
            program: laid_out,
            four_state: opts.four_state,
            layout_json,
            events_json,
            hierarchy_json,
            warnings_json,
            stable_size,
            total_size,
        })
    }

    /// Create a new simulator from a Veryl project directory.
    #[napi(factory)]
    pub fn from_project(
        project_path: String,
        top: String,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let opts = parse_options_common(&options)?;
        let (mut sources, metadata, _celox_cfg) = load_project_sources(&project_path)?;
        append_extra_source(&mut sources, &opts.extra_source);

        let source_refs: Vec<(&str, &std::path::Path)> = sources
            .iter()
            .map(|(s, p)| (s.as_str(), p.as_path()))
            .collect();

        let trace_opts = celox::TraceOptions::default();
        let (program, warnings) = celox::compile_to_sir(
            &source_refs,
            &top,
            &opts
                .false_loops
                .iter()
                .map(|(f, t)| (f.clone(), t.clone()))
                .collect::<Vec<_>>(),
            &opts
                .true_loops
                .iter()
                .map(|(f, t, m)| (f.clone(), t.clone(), *m))
                .collect::<Vec<_>>(),
            opts.four_state,
            &trace_opts,
            None,
            Some(metadata),
            opts.clock_type,
            opts.reset_type,
            &opts.parameters,
            &opts.optimize_options,
        )
        .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let laid_out = program.into_laid_out(opts.four_state);
        let layout = laid_out.layout();

        let layout_json = Self::build_layout_json(&laid_out, layout, opts.four_state);
        let events_json = Self::build_events_json(&laid_out);
        let hierarchy_json = "{}".to_string();
        let warnings_json = format_warnings_json(&warnings);

        let stable_size = layout.total_size as u32;
        let total_size = layout.merged_total_size as u32;
        Ok(Self {
            program: laid_out,
            four_state: opts.four_state,
            layout_json,
            events_json,
            hierarchy_json,
            warnings_json,
            stable_size,
            total_size,
        })
    }

    /// Returns the signal layout as a JSON string.
    #[napi(getter)]
    pub fn layout_json(&self) -> String {
        self.layout_json.clone()
    }

    /// Returns the event map as a JSON string.
    #[napi(getter)]
    pub fn events_json(&self) -> String {
        self.events_json.clone()
    }

    /// Byte ranges whose value and mask planes must start as unknown (X).
    #[napi(getter)]
    pub fn four_state_init_regions_json(&self) -> String {
        Self::build_four_state_init_regions_json(
            &self.program,
            self.program.layout(),
            self.four_state,
        )
    }

    /// Returns the instance hierarchy as a JSON string.
    #[napi(getter)]
    pub fn hierarchy_json(&self) -> String {
        self.hierarchy_json.clone()
    }

    /// Returns compilation warnings as a JSON array of strings.
    #[napi(getter)]
    pub fn warnings_json(&self) -> String {
        self.warnings_json.clone()
    }

    /// Returns the stable region size in bytes.
    #[napi(getter)]
    pub fn stable_size(&self) -> u32 {
        self.stable_size
    }

    /// Returns the total memory size in bytes.
    #[napi(getter)]
    pub fn total_size(&self) -> u32 {
        self.total_size
    }

    /// Returns the WASM module bytes for eval_comb (combinational logic evaluation).
    #[napi]
    pub fn comb_wasm_bytes(&self) -> Vec<u8> {
        let wasm = celox::wasm_codegen::compile_units(
            &self.program.sir.eval_comb,
            self.program.layout(),
            self.four_state,
            false,
        );
        wasm.bytes
    }

    /// Returns the WASM module bytes for a specific clock/reset event.
    ///
    /// `event_name` should match a clock or reset port name (e.g. "clk", "rst").
    #[napi]
    pub fn event_wasm_bytes(&self, event_name: String) -> Result<Vec<u8>> {
        for (addr, units) in &self.program.sir.eval_apply_ffs {
            let event_path = self.program.get_path(addr);
            if event_path == event_name {
                let wasm = celox::wasm_codegen::compile_units(
                    units,
                    self.program.layout(),
                    self.four_state,
                    false,
                );
                return Ok(wasm.bytes);
            }
        }

        Err(Error::from_reason(format!(
            "Event '{}' not found. Available events: {}",
            event_name,
            self.program
                .sir
                .eval_apply_ffs
                .keys()
                .map(|addr| self.program.get_path(addr))
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }

    /// No-op on wasm32 (no native resources to release).
    #[napi]
    pub fn dispose(&mut self) {}
}

#[cfg(target_arch = "wasm32")]
impl NativeSimulatorHandle {
    fn build_four_state_init_regions_json(
        program: &celox::LaidOutProgram,
        layout: &celox::MemoryLayout,
        four_state: bool,
    ) -> String {
        if !four_state {
            return "[]".to_string();
        }

        let mut regions = Vec::new();
        for (addr, &offset) in &layout.offsets {
            if program
                .design
                .state_objects
                .get(addr)
                .is_some_and(|metadata| metadata.is_4state)
            {
                regions.push((offset, layout.plane_size(addr)));
            }
        }
        for (addr, &relative_offset) in &layout.working_offsets {
            if program
                .design
                .state_objects
                .get(addr)
                .is_some_and(|metadata| metadata.is_4state)
            {
                regions.push((
                    layout.working_base_offset + relative_offset,
                    layout.plane_size(addr),
                ));
            }
        }
        regions.sort_unstable();

        serde_json::to_string(&regions).unwrap_or_else(|_| "[]".to_string())
    }

    /// Build signal layout JSON from finalized SIR and MemoryLayout.
    /// Mirrors the layout format from celox-wasm.
    fn build_layout_json(
        program: &celox::LaidOutProgram,
        layout: &celox::MemoryLayout,
        four_state: bool,
    ) -> String {
        use std::collections::BTreeMap;

        let mut layout_map: BTreeMap<String, serde_json::Value> = BTreeMap::new();

        for addr in program.design.state_objects.keys() {
            let Some(source) = program.frontend.source_address(addr) else {
                continue;
            };
            let module_id = program.frontend.instance_module[&source.instance_id];
            let variables = &program.frontend.module_variables[&module_id];
            let Some(info) = variables.get(&source.var_id) else {
                continue;
            };
            if program.frontend.module_var_path_index[&module_id].get(&info.path) == Some(&None) {
                continue;
            }
            let Some(&offset) = layout.offsets.get(addr) else {
                continue;
            };
            let name = program.get_path(addr);
            let total_width = layout.widths.get(addr).copied().unwrap_or(0);
            let (width, array_dims) = if info.array_dims.is_empty() {
                (total_width, None)
            } else {
                let element_count = info.array_dims.iter().product::<usize>();
                (total_width / element_count, Some(&info.array_dims))
            };
            let byte_size = celox::get_byte_size(width);
            let mut entry = serde_json::json!({
                "offset": offset,
                "width": width,
                "byte_size": byte_size,
                "is_4state": four_state && info.is_4state,
                "direction": layout::direction_str(info.var_kind),
                "type_kind": layout::type_kind_str(info.type_kind),
            });
            if let Some(array_dims) = array_dims {
                entry["array_dims"] = serde_json::json!(array_dims);
            }
            layout_map.insert(name, entry);
        }

        serde_json::to_string(&layout_map).unwrap_or_else(|_| "{}".to_string())
    }

    /// Build events JSON from finalized SIR.
    fn build_events_json(program: &celox::LaidOutProgram) -> String {
        use std::collections::BTreeMap;

        let mut events: BTreeMap<String, usize> = BTreeMap::new();
        let mut next_id = 0usize;

        for addr in program.sir.eval_apply_ffs.keys() {
            let name = program.get_path(addr);
            events.insert(name, next_id);
            next_id += 1;
        }

        serde_json::to_string(&events).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Convert an `AnalyzerError` to structured `JsonDiagnostic`s.
/// `all_sources` maps path → source content for offset → line:col conversion.
fn analyzer_error_to_diagnostics(
    err: &veryl_analyzer::AnalyzerError,
    all_sources: &[(String, String)],
    is_error: bool,
) -> Vec<celox_ts_gen::JsonDiagnostic> {
    use miette::Diagnostic as _;

    let severity = if is_error {
        celox_ts_gen::DiagnosticSeverity::Error
    } else {
        celox_ts_gen::DiagnosticSeverity::Warning
    };

    let message = format!("{err}");
    let help = err.help().map(|h| h.to_string());
    let url = err.url().map(|u| u.to_string());

    let labels: Vec<_> = err.labels().map(|l| l.collect()).unwrap_or_default();

    if labels.is_empty() {
        return vec![celox_ts_gen::JsonDiagnostic {
            severity,
            message,
            file: String::new(),
            line: 1,
            column: 1,
            end_line: None,
            end_column: None,
            help,
            url,
        }];
    }

    labels
        .into_iter()
        .map(|label| {
            let offset = label.offset();
            let len = label.len();

            // Find which source file this offset belongs to
            // AnalyzerError uses global offsets across concatenated sources,
            // but typically the error_location is within a single file.
            // Try each source to find line:col.
            let mut file = String::new();
            let mut line = 1usize;
            let mut col = 1usize;
            let mut end_line = None;
            let mut end_col = None;

            for (path, content) in all_sources {
                // miette offsets are per-file when Parser::parse is given the source
                let mut cur_line = 1;
                let mut cur_col = 1;
                let mut found = false;

                for (i, ch) in content.char_indices() {
                    if i == offset {
                        file = path.clone();
                        line = cur_line;
                        col = cur_col;
                        found = true;
                    }
                    if found && i == offset + len {
                        end_line = Some(cur_line);
                        end_col = Some(cur_col);
                        break;
                    }
                    if ch == '\n' {
                        cur_line += 1;
                        cur_col = 1;
                    } else {
                        cur_col += 1;
                    }
                }

                if found {
                    if end_line.is_none() {
                        end_line = Some(cur_line);
                        end_col = Some(cur_col);
                    }
                    break;
                }
            }

            celox_ts_gen::JsonDiagnostic {
                severity: severity.clone(),
                message: label.label().unwrap_or(&message).to_string(),
                file,
                line,
                column: col,
                end_line,
                end_column: end_col,
                help: help.clone(),
                url: url.clone(),
            }
        })
        .collect()
}

/// Format analyzer errors with accumulated warnings for gen_ts error messages.
fn format_errors_with_warnings(
    pass_label: &str,
    errors: &[&veryl_analyzer::AnalyzerError],
    warnings: &[veryl_analyzer::AnalyzerError],
) -> String {
    let error_msgs: Vec<String> = errors
        .iter()
        .map(|e| celox::render_diagnostic(*e))
        .collect();
    let mut msg = format!("Errors in {pass_label}: {}", error_msgs.join("; "));
    if !warnings.is_empty() {
        let warning_msgs: Vec<String> = warnings
            .iter()
            .map(|w| celox::render_diagnostic(w))
            .collect();
        msg.push_str("\n\n--- warnings ---\n\n");
        msg.push_str(&warning_msgs.join("\n"));
    }
    msg
}

/// Clear the process-global JIT compilation cache.
///
/// Call this when source files have changed and cached compiled code may be stale.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub fn clear_jit_cache() {
    let mut cache = JIT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.clear();
}

/// Stub for wasm32: no JIT cache to clear.
#[cfg(target_arch = "wasm32")]
#[napi]
pub fn clear_jit_cache() {}

// ---------------------------------------------------------------------------
//  Native testbench execution
// ---------------------------------------------------------------------------

/// Result of a single `$assert` evaluation in a native testbench.
#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiAssertionResult {
    pub passed: bool,
    pub message: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// Detailed result of running a native testbench.
#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiTestResult {
    pub passed: bool,
    pub assertions: Vec<NapiAssertionResult>,
    pub error: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
fn convert_test_result(r: celox::TestResultDetailed) -> NapiTestResult {
    NapiTestResult {
        passed: r.passed,
        error: r.error,
        assertions: r
            .assertions
            .into_iter()
            .map(|a| {
                let (file, line, column) = match a.location {
                    Some(loc) => (Some(loc.file), Some(loc.line), Some(loc.column)),
                    None => (None, None, None),
                };
                NapiAssertionResult {
                    passed: a.passed,
                    message: a.message,
                    file,
                    line,
                    column,
                }
            })
            .collect(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiInjectedValue {
    pub name: Option<String>,
    pub bits: Option<BigInt>,
    pub mask_xz: Option<BigInt>,
    pub width: Option<u32>,
    pub string_value: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiInjectedCall {
    pub instance: String,
    pub phase: String,
    pub method: Option<String>,
    pub inputs: Vec<NapiInjectedValue>,
    pub params: Vec<NapiInjectedValue>,
    pub ports: Vec<NapiInjectedPort>,
    pub args: Vec<NapiInjectedValue>,
    pub cycle: BigInt,
    pub time: BigInt,
    pub seed: BigInt,
    pub fired_clock: Option<String>,
    pub four_state: bool,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiInjectedPort {
    pub name: String,
    pub direction: String,
    pub role: Option<String>,
    pub width: u32,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiInjectedResult {
    pub outputs: Option<Vec<NapiInjectedValue>>,
    pub return_value: Option<NapiInjectedValue>,
    pub failures: Option<Vec<String>>,
    pub logs: Option<Vec<String>>,
    pub finish: Option<bool>,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi(object, object_to_js = false)]
pub struct NapiInjectedComponent {
    pub name: String,
    pub manifest: String,
    pub handler: FunctionRef<NapiInjectedCall, NapiInjectedResult>,
}

#[cfg(not(target_arch = "wasm32"))]
struct NapiInjectedHandler {
    env: Env,
    handler: FunctionRef<NapiInjectedCall, NapiInjectedResult>,
}

// Injected callbacks are only accepted by the synchronous runTest APIs and
// are invoked on the JS thread which supplied this Env. The core trait is
// Send + Sync because compiled component hooks may otherwise be movable.
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Send for NapiInjectedHandler {}
#[cfg(not(target_arch = "wasm32"))]
unsafe impl Sync for NapiInjectedHandler {}

#[cfg(not(target_arch = "wasm32"))]
fn napi_bigint(value: u64) -> BigInt {
    BigInt {
        sign_bit: false,
        words: vec![value],
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn to_napi_injected_value(name: Option<String>, value: celox::InjectedValue) -> NapiInjectedValue {
    match value {
        celox::InjectedValue::Bits {
            words,
            mask_xz,
            width,
        } => NapiInjectedValue {
            name,
            bits: Some(BigInt {
                sign_bit: false,
                words,
            }),
            mask_xz: Some(BigInt {
                sign_bit: false,
                words: mask_xz,
            }),
            width: Some(width),
            string_value: None,
        },
        celox::InjectedValue::String(value) => NapiInjectedValue {
            name,
            bits: None,
            mask_xz: None,
            width: None,
            string_value: Some(value),
        },
        celox::InjectedValue::Unit => NapiInjectedValue {
            name,
            bits: None,
            mask_xz: None,
            width: None,
            string_value: None,
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn from_napi_injected_value(
    value: NapiInjectedValue,
) -> std::result::Result<celox::InjectedValue, String> {
    if let Some(bits) = value.bits {
        if bits.sign_bit || value.mask_xz.as_ref().is_some_and(|mask| mask.sign_bit) {
            return Err("component callback values cannot be negative".into());
        }
        let width = value
            .width
            .ok_or_else(|| "component callback bit value has no width".to_string())?;
        return Ok(celox::InjectedValue::Bits {
            words: bits.words,
            mask_xz: value.mask_xz.map(|mask| mask.words).unwrap_or_default(),
            width,
        });
    }
    Ok(match value.string_value {
        Some(value) => celox::InjectedValue::String(value),
        None => celox::InjectedValue::Unit,
    })
}

#[cfg(not(target_arch = "wasm32"))]
impl celox::InjectedComponentHandler for NapiInjectedHandler {
    fn call(
        &self,
        call: celox::InjectedCall,
    ) -> std::result::Result<celox::InjectedResult, String> {
        let (phase, method, args) = match call.hook {
            celox::InjectedHook::Create => ("create", None, Vec::new()),
            celox::InjectedHook::Init => ("init", None, Vec::new()),
            celox::InjectedHook::Reset => ("reset", None, Vec::new()),
            celox::InjectedHook::Clock => ("clock", None, Vec::new()),
            celox::InjectedHook::Finish => ("finish", None, Vec::new()),
            celox::InjectedHook::Method { name, args } => ("method", Some(name), args),
        };
        let request = NapiInjectedCall {
            instance: call.instance,
            phase: phase.into(),
            method,
            inputs: call
                .inputs
                .into_iter()
                .map(|value| to_napi_injected_value(Some(value.name), value.value))
                .collect(),
            params: call
                .params
                .into_iter()
                .map(|value| to_napi_injected_value(Some(value.name), value.value))
                .collect(),
            ports: call
                .ports
                .into_iter()
                .map(|port| NapiInjectedPort {
                    name: port.name,
                    direction: port.direction,
                    role: port.role,
                    width: port.width,
                })
                .collect(),
            args: args
                .into_iter()
                .map(|value| to_napi_injected_value(None, value))
                .collect(),
            cycle: napi_bigint(call.cycle),
            time: napi_bigint(call.time),
            seed: napi_bigint(call.seed),
            fired_clock: call.fired_clock,
            four_state: call.four_state,
        };
        let result = self
            .handler
            .borrow_back(&self.env)
            .and_then(|handler| handler.call(request))
            .map_err(|error| error.to_string())?;
        let outputs = result
            .outputs
            .unwrap_or_default()
            .into_iter()
            .map(|value| {
                let name = value
                    .name
                    .clone()
                    .ok_or_else(|| "component callback output has no name".to_string())?;
                Ok(celox::InjectedNamedValue {
                    name,
                    value: from_napi_injected_value(value)?,
                })
            })
            .collect::<std::result::Result<Vec<_>, String>>()?;
        Ok(celox::InjectedResult {
            outputs,
            return_value: result
                .return_value
                .map(from_napi_injected_value)
                .transpose()?,
            failures: result.failures.unwrap_or_default(),
            logs: result.logs.unwrap_or_default(),
            finish: result.finish.unwrap_or(false),
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn injected_components(
    env: Env,
    definitions: Option<Vec<NapiInjectedComponent>>,
) -> Result<celox::InjectedComponents> {
    let mut components = celox::InjectedComponents::new();
    for definition in definitions.unwrap_or_default() {
        components
            .insert(
                definition.name,
                &definition.manifest,
                Arc::new(NapiInjectedHandler {
                    env,
                    handler: definition.handler,
                }),
            )
            .map_err(Error::from_reason)?;
    }
    Ok(components)
}

/// Run a native testbench from Veryl source code.
///
/// Compiles the given sources and runs the `#[test]` module specified by `top`,
/// returning assertion results observed before that test finishes or stops on a
/// fatal failure.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub fn run_test(
    env: Env,
    sources: Vec<NapiSourceFile>,
    top: String,
    options: Option<NapiOptions>,
    components: Option<Vec<NapiInjectedComponent>>,
) -> Result<NapiTestResult> {
    let opts = parse_options(&options)?;
    let mut src_pairs: Vec<(String, std::path::PathBuf)> = sources
        .into_iter()
        .map(|s| (s.content, std::path::PathBuf::from(s.path)))
        .collect();
    append_extra_source(&mut src_pairs, &opts.extra_source);

    let source_refs: Vec<(&str, &std::path::Path)> = src_pairs
        .iter()
        .map(|(s, p)| (s.as_str(), p.as_path()))
        .collect();
    let builder = apply_options(celox::Simulator::from_sources(source_refs, &top), &opts)
        .with_injected_components(injected_components(env, components)?);
    let result = builder
        .run_test_detailed()
        .map_err(|e| Error::from_reason(format!("{e}")))?;
    Ok(convert_test_result(result))
}

/// Run a native testbench from a Veryl project directory.
///
/// Searches upward from `project_path` for `Veryl.toml`, gathers all
/// `.veryl` source files, and runs the `#[test]` module specified by `top`.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub fn run_test_from_project(
    env: Env,
    project_path: String,
    top: String,
    options: Option<NapiOptions>,
    components: Option<Vec<NapiInjectedComponent>>,
) -> Result<NapiTestResult> {
    let opts = parse_options(&options)?;
    let (mut sources, metadata, _celox_cfg) = load_project_sources(&project_path)?;
    append_extra_source(&mut sources, &opts.extra_source);

    let source_refs: Vec<(&str, &std::path::Path)> = sources
        .iter()
        .map(|(s, p)| (s.as_str(), p.as_path()))
        .collect();
    let builder = apply_options(
        celox::Simulator::from_sources(source_refs, &top).with_metadata(metadata),
        &opts,
    )
    .with_injected_components(injected_components(env, components)?);
    let result = builder
        .run_test_detailed()
        .map_err(|e| Error::from_reason(format!("{e}")))?;
    Ok(convert_test_result(result))
}

// ---------------------------------------------------------------------------
//  TypeScript type generation
// ---------------------------------------------------------------------------

#[napi(object)]
pub struct NapiInjectedManifest {
    pub name: String,
    pub manifest: String,
}

fn inject_analyzer_components(definitions: Option<Vec<NapiInjectedManifest>>) -> Result<()> {
    let definitions = definitions.unwrap_or_default();
    let names: Vec<_> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    veryl_analyzer::tb_component::insert_external_components(&names);
    for definition in definitions {
        let manifest =
            veryl_metadata::ComponentManifest::parse(&definition.manifest).ok_or_else(|| {
                Error::from_reason(format!(
                    "manifest of injected component `{}` cannot be parsed",
                    definition.name
                ))
            })?;
        veryl_analyzer::component_manifest_table::insert(
            veryl_parser::resource_table::insert_str(&definition.name),
            manifest,
        );
    }
    Ok(())
}

/// Generate TypeScript type information as JSON for a Veryl project.
///
/// Equivalent to running `celox-gen-ts --json` from the given project directory.
#[napi]
pub fn gen_ts(
    project_path: String,
    components: Option<Vec<NapiInjectedManifest>>,
) -> Result<String> {
    use celox_ts_gen::{JsonModuleEntry, JsonOutput, generate_all};

    let toml_path = Metadata::search_from(&project_path)
        .map_err(|e| Error::from_reason(format!("Could not find Veryl.toml: {e}")))?;
    let mut metadata = Metadata::load(&toml_path)
        .map_err(|e| Error::from_reason(format!("Failed to load Veryl.toml: {e}")))?;

    let base_path = toml_path
        .parent()
        .unwrap_or(&toml_path)
        .to_string_lossy()
        .to_string();

    let mut paths = metadata
        .paths::<std::path::PathBuf>(&[], true, true)
        .map_err(|e| Error::from_reason(format!("Failed to gather sources: {e}")))?;
    paths.retain(|path| !path.example);

    // Append test-only sources declared in celox.toml
    let project_root = toml_path.parent().unwrap_or(&toml_path).to_path_buf();
    let celox_cfg = load_celox_config(&project_root)?;
    let prj_name = metadata.project.name.clone();
    for dir in &celox_cfg.test.sources {
        let dir_path = project_root.join(dir);
        if !dir_path.exists() {
            continue;
        }
        for src in walkdir(&dir_path)? {
            paths.push(PathSet {
                prj: prj_name.clone(),
                src: src.clone(),
                dst: src.with_extension("sv"),
                map: src.with_extension("map"),
                example: false,
            });
        }
    }

    if let Some(exclude_set) = build_exclude_set(&celox_cfg)? {
        paths.retain(|p| !is_excluded(&p.src, &project_root, &exclude_set));
    }

    if paths.is_empty() {
        return Err(Error::from_reason("No Veryl source files found"));
    }

    // Parse and analyze pass 1
    symbol_table::clear();
    attribute_table::clear();

    let analyzer = Analyzer::new(&metadata);
    inject_analyzer_components(components)?;
    let mut parsers = Vec::new();
    let mut all_warnings = Vec::new();

    for path in &paths {
        let input = std::fs::read_to_string(&path.src)
            .map_err(|e| Error::from_reason(format!("{}: {e}", path.src.display())))?;
        let parser = Parser::parse(&input, &path.src)
            .map_err(|e| Error::from_reason(format!("Parse error: {e}")))?;

        let results = analyzer.analyze_pass1(&path.prj, &parser.veryl);
        let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
        if !real_errors.is_empty() {
            return Err(Error::from_reason(format_errors_with_warnings(
                "analysis pass 1",
                &real_errors,
                &all_warnings,
            )));
        }
        all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

        parsers.push((path.clone(), parser));
    }

    let results = Analyzer::analyze_post_pass1();
    let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
    if !real_errors.is_empty() {
        return Err(Error::from_reason(format_errors_with_warnings(
            "post-pass 1 analysis",
            &real_errors,
            &all_warnings,
        )));
    }
    all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

    // Pass 2: per-file IR → generate

    // Compute all source file relative paths for embedding in generated JS.
    let base_normalized = base_path.replace('\\', "/");
    let all_source_files: Vec<String> = parsers
        .iter()
        .map(|(path, _)| {
            let src_normalized = path
                .src
                .to_string_lossy()
                .replace(r"\\?\", "")
                .replace('\\', "/");
            src_normalized
                .strip_prefix(&base_normalized)
                .unwrap_or(&src_normalized)
                .trim_start_matches('/')
                .to_string()
        })
        .collect();
    let source_file_refs: Vec<&str> = all_source_files.iter().map(|s| s.as_str()).collect();

    let mut all_modules = Vec::new();
    let mut file_modules: HashMap<String, Vec<String>> = HashMap::default();
    let mut post_pass_ir = Ir::default();

    for (i, (_path, parser)) in parsers.iter().enumerate() {
        let mut analyzer_context = Context::default();
        let mut ir = Ir::default();
        let results = analyzer.analyze_pass2(&parser.veryl, &mut analyzer_context, Some(&mut ir));
        let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
        if !real_errors.is_empty() {
            return Err(Error::from_reason(format_errors_with_warnings(
                "analysis pass 2",
                &real_errors,
                &all_warnings,
            )));
        }
        all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

        let modules = generate_all(&ir, &source_file_refs);
        post_pass_ir.append(&mut ir);
        let source_file = all_source_files[i].clone();

        let module_names: Vec<String> = modules.iter().map(|m| m.module_name.clone()).collect();
        if !module_names.is_empty() {
            file_modules.insert(source_file.clone(), module_names);
        }

        for m in modules {
            all_modules.push(JsonModuleEntry {
                module_name: m.module_name,
                source_file: source_file.clone(),
                dts_content: m.dts_content,
                md_content: m.md_content,
                ports: m.ports,
                events: m.events,
                instances: m.instances,
                is_test: m.is_test,
            });
        }
    }

    let results = Analyzer::analyze_post_pass2(&post_pass_ir);
    let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
    if !real_errors.is_empty() {
        return Err(Error::from_reason(format_errors_with_warnings(
            "post-pass 2 analysis",
            &real_errors,
            &all_warnings,
        )));
    }
    all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

    // Sort for deterministic output
    all_modules.sort_by(|a, b| a.module_name.cmp(&b.module_name));

    let warning_msgs: Vec<String> = all_warnings
        .iter()
        .map(|w| celox::render_diagnostic(w))
        .collect();

    let all_sources: Vec<(String, String)> = parsers
        .iter()
        .map(|(p, _)| {
            let path_str = p.src.to_string_lossy().to_string();
            let content = std::fs::read_to_string(&p.src).unwrap_or_default();
            (path_str, content)
        })
        .collect();

    let diagnostics: Vec<celox_ts_gen::JsonDiagnostic> = all_warnings
        .iter()
        .flat_map(|w| analyzer_error_to_diagnostics(w, &all_sources, false))
        .collect();

    let output = JsonOutput {
        project_path: base_path,
        modules: all_modules,
        file_modules,
        warnings: warning_msgs,
        diagnostics,
    };

    serde_json::to_string(&output)
        .map_err(|e| Error::from_reason(format!("Failed to serialize JSON: {e}")))
}

/// Generate TypeScript type information from in-memory Veryl sources.
///
/// Like `gen_ts()` but does not require a Veryl.toml or filesystem access.
/// Works on both native and wasm32 targets.
#[napi]
pub fn gen_ts_from_source(
    sources: Vec<NapiSourceFile>,
    components: Option<Vec<NapiInjectedManifest>>,
) -> Result<String> {
    use celox_ts_gen::{JsonModuleEntry, JsonOutput, generate_all};

    if sources.is_empty() {
        return Err(Error::from_reason("No source files provided"));
    }

    let metadata = Metadata::create_default("playground")
        .map_err(|e| Error::from_reason(format!("Failed to create default metadata: {e}")))?;

    // Parse and analyze pass 1
    symbol_table::clear();
    attribute_table::clear();

    let analyzer = Analyzer::new(&metadata);
    inject_analyzer_components(components)?;
    let mut parsers = Vec::new();
    let mut all_warnings = Vec::new();

    for src in &sources {
        let path = std::path::PathBuf::from(&src.path);
        let parser = Parser::parse(&src.content, &path)
            .map_err(|e| Error::from_reason(format!("Parse error: {e}")))?;

        let prj = "playground".to_string();
        let results = analyzer.analyze_pass1(&prj, &parser.veryl);
        let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
        if !real_errors.is_empty() {
            return Err(Error::from_reason(format_errors_with_warnings(
                "analysis pass 1",
                &real_errors,
                &all_warnings,
            )));
        }
        all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

        parsers.push((prj, path, parser));
    }

    let results = Analyzer::analyze_post_pass1();
    let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
    if !real_errors.is_empty() {
        return Err(Error::from_reason(format_errors_with_warnings(
            "post-pass 1 analysis",
            &real_errors,
            &all_warnings,
        )));
    }
    all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

    // Pass 2: per-file IR → generate
    let all_source_files: Vec<String> = parsers
        .iter()
        .map(|(_, p, _)| p.to_string_lossy().replace('\\', "/"))
        .collect();
    let source_file_refs: Vec<&str> = all_source_files.iter().map(|s| s.as_str()).collect();

    let mut all_modules = Vec::new();
    let mut file_modules: HashMap<String, Vec<String>> = HashMap::default();
    let mut post_pass_ir = Ir::default();

    for (i, (_prj, _path, parser)) in parsers.iter().enumerate() {
        let mut analyzer_context = Context::default();
        let mut ir = Ir::default();
        let results = analyzer.analyze_pass2(&parser.veryl, &mut analyzer_context, Some(&mut ir));
        let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
        if !real_errors.is_empty() {
            return Err(Error::from_reason(format_errors_with_warnings(
                "analysis pass 2",
                &real_errors,
                &all_warnings,
            )));
        }
        all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

        let modules = generate_all(&ir, &source_file_refs);
        post_pass_ir.append(&mut ir);
        let source_file = all_source_files[i].clone();

        let module_names: Vec<String> = modules.iter().map(|m| m.module_name.clone()).collect();
        if !module_names.is_empty() {
            file_modules.insert(source_file.clone(), module_names);
        }

        for m in modules {
            all_modules.push(JsonModuleEntry {
                module_name: m.module_name,
                source_file: source_file.clone(),
                dts_content: m.dts_content,
                md_content: m.md_content,
                ports: m.ports,
                events: m.events,
                instances: m.instances,
                is_test: m.is_test,
            });
        }
    }

    let results = Analyzer::analyze_post_pass2(&post_pass_ir);
    let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
    if !real_errors.is_empty() {
        return Err(Error::from_reason(format_errors_with_warnings(
            "post-pass 2 analysis",
            &real_errors,
            &all_warnings,
        )));
    }
    all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

    // Sort for deterministic output
    all_modules.sort_by(|a, b| a.module_name.cmp(&b.module_name));

    let warning_msgs: Vec<String> = all_warnings
        .iter()
        .map(|w| celox::render_diagnostic(w))
        .collect();

    let all_sources: Vec<(String, String)> = sources
        .iter()
        .map(|s| (s.path.clone(), s.content.clone()))
        .collect();

    let diagnostics: Vec<celox_ts_gen::JsonDiagnostic> = all_warnings
        .iter()
        .flat_map(|w| analyzer_error_to_diagnostics(w, &all_sources, false))
        .collect();

    let output = JsonOutput {
        project_path: String::new(),
        modules: all_modules,
        file_modules,
        warnings: warning_msgs,
        diagnostics,
    };

    serde_json::to_string(&output)
        .map_err(|e| Error::from_reason(format!("Failed to serialize JSON: {e}")))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn default_opts() -> ParsedOptions {
        ParsedOptions {
            common: ParsedOptionsCommon {
                four_state: false,
                optimize_options: celox::OptimizeOptions::all(),
                vcd: None,
                false_loops: vec![],
                true_loops: vec![],
                clock_type: None,
                reset_type: None,
                extra_source: None,
                parameters: vec![],
            },
            cranelift_options: celox::CraneliftOptions::default(),
            dead_store_policy: celox::DeadStorePolicy::Off,
        }
    }

    fn make_sources(pairs: &[(&str, &str)]) -> Vec<(String, std::path::PathBuf)> {
        pairs
            .iter()
            .map(|(content, path)| (content.to_string(), std::path::PathBuf::from(path)))
            .collect()
    }

    #[test]
    fn normal_project_loading_excludes_example_sources() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join("src")).unwrap();
        std::fs::create_dir(project.path().join("examples")).unwrap();
        std::fs::write(
            project.path().join("Veryl.toml"),
            "[project]\nname = \"example_filter\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(project.path().join("src/top.veryl"), "module Top () {}\n").unwrap();
        std::fs::write(
            project.path().join("examples/demo.veryl"),
            "module Demo () {}\n",
        )
        .unwrap();

        let (sources, _, _) = load_project_sources(project.path().to_str().unwrap()).unwrap();
        let source_names = sources
            .iter()
            .map(|(_, path)| path.file_name().unwrap().to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(source_names, ["top.veryl"]);
    }

    #[test]
    fn same_inputs_produce_same_key() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let opts = default_opts();
        let k1 = build_cache_key(&src, "Top", &opts, None);
        let k2 = build_cache_key(&src, "Top", &opts, None);
        assert_eq!(k1, k2);
    }

    #[test]
    fn high_index_sir_pass_changes_cache_key_without_bit_packing() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let enabled = default_opts();
        let mut disabled = default_opts();
        disabled.common.optimize_options = disabled
            .common
            .optimize_options
            .clone()
            .disable(celox::SirPass::IdentityStoreBypass);

        assert_ne!(
            build_cache_key(&src, "Top", &enabled, None),
            build_cache_key(&src, "Top", &disabled, None)
        );
    }

    #[test]
    fn native_memory_width_changes_cache_key() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut narrow = default_opts();
        narrow.common.optimize_options = narrow
            .common
            .optimize_options
            .clone()
            .with_max_native_memory_width(64);
        let mut wide = default_opts();
        wide.common.optimize_options = wide
            .common
            .optimize_options
            .clone()
            .with_max_native_memory_width(128);

        assert_ne!(
            build_cache_key(&src, "Top", &narrow, None),
            build_cache_key(&src, "Top", &wide, None)
        );
    }

    #[test]
    fn different_source_content_different_key() {
        let s1 = make_sources(&[("module A {}", "a.veryl")]);
        let s2 = make_sources(&[("module B {}", "a.veryl")]);
        let opts = default_opts();
        assert_ne!(
            build_cache_key(&s1, "Top", &opts, None),
            build_cache_key(&s2, "Top", &opts, None),
        );
    }

    #[test]
    fn different_source_path_different_key() {
        let s1 = make_sources(&[("module A {}", "a.veryl")]);
        let s2 = make_sources(&[("module A {}", "b.veryl")]);
        let opts = default_opts();
        assert_ne!(
            build_cache_key(&s1, "Top", &opts, None),
            build_cache_key(&s2, "Top", &opts, None),
        );
    }

    #[test]
    fn different_top_different_key() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let opts = default_opts();
        assert_ne!(
            build_cache_key(&src, "Top", &opts, None),
            build_cache_key(&src, "Other", &opts, None),
        );
    }

    #[test]
    fn four_state_differs() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        o1.common.four_state = false;
        o2.common.four_state = true;
        assert_ne!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn optimize_options_differs() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        o1.common.optimize_options = celox::OptimizeOptions::all();
        o2.common.optimize_options = celox::OptimizeOptions::none();
        assert_ne!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn cranelift_opt_level_differs() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        o1.cranelift_options.opt_level = celox::CraneliftOptLevel::Speed;
        o2.cranelift_options.opt_level = celox::CraneliftOptLevel::None;
        assert_ne!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn regalloc_algorithm_differs() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        o1.cranelift_options.regalloc_algorithm = celox::RegallocAlgorithm::Backtracking;
        o2.cranelift_options.regalloc_algorithm = celox::RegallocAlgorithm::SinglePass;
        assert_ne!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn dead_store_policy_differs() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        o1.dead_store_policy = celox::DeadStorePolicy::Off;
        o2.dead_store_policy = celox::DeadStorePolicy::PreserveTopPorts;
        assert_ne!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn clock_type_differs() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        o1.common.clock_type = None;
        o2.common.clock_type = Some(celox::ClockType::NegEdge);
        assert_ne!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn reset_type_differs() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        o1.common.reset_type = None;
        o2.common.reset_type = Some(celox::ResetType::SyncHigh);
        assert_ne!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn parameters_differ() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        o1.common.parameters = vec![("WIDTH".into(), 8)];
        o2.common.parameters = vec![("WIDTH".into(), 16)];
        assert_ne!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn source_order_independent() {
        let s1 = make_sources(&[("aaa", "a.veryl"), ("bbb", "b.veryl")]);
        let s2 = make_sources(&[("bbb", "b.veryl"), ("aaa", "a.veryl")]);
        let opts = default_opts();
        assert_eq!(
            build_cache_key(&s1, "Top", &opts, None),
            build_cache_key(&s2, "Top", &opts, None),
        );
    }

    #[test]
    fn non_compilation_options_ignored() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        // VCD path doesn't affect compilation
        o1.common.vcd = None;
        o2.common.vcd = Some("/tmp/dump.vcd".into());
        assert_eq!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn false_loops_differ() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        o1.common.false_loops = vec![];
        o2.common.false_loops = vec![((vec![], vec!["a".into()]), (vec![], vec!["b".into()]))];
        assert_ne!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn true_loops_differ() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let mut o1 = default_opts();
        let mut o2 = default_opts();
        o1.common.true_loops = vec![];
        o2.common.true_loops = vec![((vec![], vec!["x".into()]), (vec![], vec!["y".into()]), 4)];
        assert_ne!(
            build_cache_key(&src, "Top", &o1, None),
            build_cache_key(&src, "Top", &o2, None),
        );
    }

    #[test]
    fn metadata_clock_reset_differs() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let opts = default_opts();

        let mut m1 = Metadata::create_default("prj").unwrap();
        let mut m2 = Metadata::create_default("prj").unwrap();
        m1.build.clock_type = celox::ClockType::PosEdge;
        m2.build.clock_type = celox::ClockType::NegEdge;
        assert_ne!(
            build_cache_key(&src, "Top", &opts, Some(&m1)),
            build_cache_key(&src, "Top", &opts, Some(&m2)),
        );

        let mut m3 = Metadata::create_default("prj").unwrap();
        let mut m4 = Metadata::create_default("prj").unwrap();
        m3.build.reset_type = celox::ResetType::AsyncLow;
        m4.build.reset_type = celox::ResetType::SyncHigh;
        assert_ne!(
            build_cache_key(&src, "Top", &opts, Some(&m3)),
            build_cache_key(&src, "Top", &opts, Some(&m4)),
        );
    }

    #[test]
    fn no_metadata_vs_metadata_differs() {
        let src = make_sources(&[("module Top {}", "a.veryl")]);
        let opts = default_opts();
        let m = Metadata::create_default("prj").unwrap();
        // No metadata vs with metadata should differ (metadata adds clock/reset info)
        assert_ne!(
            build_cache_key(&src, "Top", &opts, None),
            build_cache_key(&src, "Top", &opts, Some(&m)),
        );
    }
}
