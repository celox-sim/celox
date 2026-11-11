//! Options of a build as JavaScript passes them, and their conversion.

use super::*;

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
    /// Ordered SystemVerilog command-line arguments (leading `+` optional).
    pub plusargs: Option<Vec<String>>,
    /// Permit inferred latches when inferred_latch has no explicit diagnostic level.
    /// Explicit project diagnostics take precedence. Default: false.
    pub allow_inferred_latches: Option<bool>,
    pub vcd: Option<String>,
    /// Path of the FST waveform file to record; mutually exclusive with `vcd`.
    pub fst: Option<String>,
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
    /// Simulation threads, including the calling thread. Default: 1.
    pub threads: Option<u32>,
    /// Tiered handles compile hot execution groups incrementally (hotspot
    /// JIT). Default: true; `false` selects whole-image tiered compilation.
    pub hotspot_jit: Option<bool>,
}

/// Parsed builder options from NapiOptions (common fields available on all targets).
#[allow(dead_code)]
pub(super) struct ParsedOptionsCommon {
    pub(super) diagnostic_policy: celox::DiagnosticPolicy,
    pub(super) four_state: bool,
    pub(super) plusargs: Vec<String>,
    pub(super) allow_inferred_latches: bool,
    pub(super) optimize_options: celox::OptimizeOptions,
    pub(super) waveform: Option<(String, celox::WaveformFormat)>,
    pub(super) false_loops: Vec<(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )>,
    pub(super) true_loops: Vec<(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )>,
    pub(super) clock_type: Option<celox::ClockType>,
    pub(super) reset_type: Option<celox::ResetType>,
    pub(super) extra_source: Option<String>,
    pub(super) parameters: Vec<(String, u64)>,
}

/// Parsed builder options from NapiOptions (native-only, includes Cranelift/DSE options).
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct ParsedOptions {
    pub(super) common: ParsedOptionsCommon,
    pub(super) cranelift_options: celox::CraneliftOptions,
    pub(super) dead_store_policy: celox::DeadStorePolicy,
    pub(super) hotspot_jit: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl std::ops::Deref for ParsedOptions {
    type Target = ParsedOptionsCommon;
    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

/// Convert a NapiSignalPath to the Rust builder's tuple format.
pub(super) fn convert_signal_path(p: &NapiSignalPath) -> (Vec<(String, usize)>, Vec<String>) {
    let inst: Vec<(String, usize)> = p
        .instance_path
        .iter()
        .map(|seg| (seg.name.clone(), seg.index as usize))
        .collect();
    let var_path: Vec<String> = p.var_path.clone();
    (inst, var_path)
}

/// Parse a clock type string into ClockType.
pub(super) fn parse_clock_type(s: &str) -> Result<celox::ClockType> {
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
pub(super) fn parse_reset_type(s: &str) -> Result<celox::ResetType> {
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
pub(super) fn convert_optimize_options(napi: &NapiOptimizeOptions) -> celox::OptimizeOptions {
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
pub(super) fn apply_pass_overrides(
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
pub(super) fn parse_cranelift_opt_level(s: &str) -> Result<celox::CraneliftOptLevel> {
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
pub(super) fn parse_regalloc_algorithm(s: &str) -> Result<celox::RegallocAlgorithm> {
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
pub(super) fn parse_dead_store_policy(s: &str) -> Result<celox::DeadStorePolicy> {
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

/// Resolve the `vcd`/`fst` options into one waveform destination.
pub(super) fn parse_waveform_option(
    options: &NapiOptions,
) -> Result<Option<(String, celox::WaveformFormat)>> {
    match (&options.vcd, &options.fst) {
        (Some(_), Some(_)) => Err(Error::from_reason(
            "options.vcd and options.fst are mutually exclusive",
        )),
        (Some(path), None) => Ok(Some((path.clone(), celox::WaveformFormat::Vcd))),
        (None, Some(path)) => Ok(Some((path.clone(), celox::WaveformFormat::Fst))),
        (None, None) => Ok(None),
    }
}

impl ParsedOptionsCommon {
    /// The waveform destination for native handles; WASM handles do not
    /// take a host file.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn waveform_arg(&self) -> Option<(&str, celox::WaveformFormat)> {
        self.waveform
            .as_ref()
            .map(|(path, format)| (path.as_str(), *format))
    }
}

/// Helper to extract the common builder config from NapiOptions (available on all targets).
pub(super) fn parse_options_common(options: &Option<NapiOptions>) -> Result<ParsedOptionsCommon> {
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
                diagnostic_policy: Default::default(),
                four_state: o.four_state.unwrap_or(false),
                plusargs: o.plusargs.clone().unwrap_or_default(),
                allow_inferred_latches: o.allow_inferred_latches.unwrap_or(false),
                optimize_options,
                waveform: parse_waveform_option(o)?,
                false_loops,
                true_loops,
                clock_type,
                reset_type,
                extra_source: o.extra_source.clone(),
                parameters,
            })
        }
        None => Ok(ParsedOptionsCommon {
            diagnostic_policy: Default::default(),
            four_state: false,
            plusargs: Vec::new(),
            allow_inferred_latches: false,
            optimize_options: celox::OptimizeOptions::all(),
            waveform: None,
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
pub(super) fn parse_options(options: &Option<NapiOptions>) -> Result<ParsedOptions> {
    let mut common = parse_options_common(options)?;
    match options.as_ref() {
        Some(o) => {
            if let Some(threads) = o.threads {
                if threads == 0 {
                    return Err(Error::from_reason(
                        "Invalid threads 0. Expected at least 1.".to_string(),
                    ));
                }
                common
                    .optimize_options
                    .set_parallel_lanes(threads.min(celox::MAX_SIMULATION_THREADS));
            }
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
                hotspot_jit: o.hotspot_jit.unwrap_or(true),
            })
        }
        None => Ok(ParsedOptions {
            common,
            cranelift_options: celox::CraneliftOptions::default(),
            dead_store_policy: celox::DeadStorePolicy::Off,
            hotspot_jit: true,
        }),
    }
}

/// Apply parsed options to a SimulatorBuilder.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn apply_options<'a, T>(
    mut builder: celox::SimulatorBuilder<'a, T>,
    opts: &ParsedOptions,
) -> celox::SimulatorBuilder<'a, T> {
    builder = builder.diagnostic_policy(
        opts.diagnostic_policy
            .clone()
            .with_inferred_latches(opts.allow_inferred_latches),
    );
    builder = builder.four_state(opts.four_state);
    builder = builder.plusargs(opts.plusargs.clone());
    builder = builder.optimize_options(opts.optimize_options.clone());
    builder = builder.threads(opts.optimize_options.parallel_lanes() as usize);
    builder = builder.cranelift_options(opts.cranelift_options);
    // Waveform recording changes the layout and generated stores, so
    // enable it before building.
    if let Some((path, format)) = opts.waveform.as_ref() {
        builder = match format {
            celox::WaveformFormat::Vcd => builder.vcd(path),
            celox::WaveformFormat::Fst => builder.fst(path),
        };
    }
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
    // Only tiered builds read it; compiled images are unaffected.
    builder = builder.hotspot_jit(opts.hotspot_jit);
    builder
}
