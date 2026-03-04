mod layout;

use std::collections::HashMap;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use veryl_analyzer::{Analyzer, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::Parser;
use veryl_path::PathSet;

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

/// Options for creating a simulator/simulation handle.
#[napi(object)]
pub struct NapiOptions {
    pub four_state: Option<bool>,
    pub vcd: Option<String>,
    pub optimize: Option<bool>,
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

/// Parsed builder options from NapiOptions.
struct ParsedOptions {
    four_state: bool,
    optimize: Option<bool>,
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
    dead_store_policy: celox::DeadStorePolicy,
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

/// Parse a dead store policy string into DeadStorePolicy.
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

/// Helper to extract the builder config from NapiOptions.
fn parse_options(options: &Option<NapiOptions>) -> Result<ParsedOptions> {
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
            let dead_store_policy = o
                .dead_store_policy
                .as_deref()
                .map(parse_dead_store_policy)
                .transpose()?
                .unwrap_or(celox::DeadStorePolicy::Off);
            Ok(ParsedOptions {
                four_state: o.four_state.unwrap_or(false),
                optimize: o.optimize,
                vcd: o.vcd.clone(),
                false_loops,
                true_loops,
                clock_type,
                reset_type,
                extra_source: o.extra_source.clone(),
                parameters,
                dead_store_policy,
            })
        }
        None => Ok(ParsedOptions {
            four_state: false,
            optimize: None,
            vcd: None,
            false_loops: Vec::new(),
            true_loops: Vec::new(),
            clock_type: None,
            reset_type: None,
            extra_source: None,
            parameters: Vec::new(),
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
struct CeloxConfig {
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
    for p in &paths {
        let content = std::fs::read_to_string(&p.src)
            .map_err(|e| Error::from_reason(format!("{}: {e}", p.src.display())))?;
        sources.push((content, p.src.clone()));
    }
    let project_root = toml_path.parent().unwrap_or(&toml_path);
    let celox_cfg = load_celox_config(project_root)?;
    collect_test_sources(&mut sources, project_root, &celox_cfg)?;
    Ok((sources, metadata, celox_cfg))
}

/// Format analyzer warnings as a JSON array of strings.
fn format_warnings_json(warnings: &[veryl_analyzer::AnalyzerError]) -> String {
    let msgs: Vec<String> = warnings.iter().map(|w| format!("{w}")).collect();
    serde_json::to_string(&msgs).unwrap_or_else(|_| "[]".to_string())
}

/// Apply parsed options to a SimulatorBuilder.
fn apply_options<'a, T>(
    mut builder: celox::SimulatorBuilder<'a, T>,
    opts: &ParsedOptions,
) -> celox::SimulatorBuilder<'a, T> {
    builder = builder.four_state(opts.four_state);
    if let Some(opt) = opts.optimize {
        builder = builder.optimize(opt);
    }
    if let Some(path) = &opts.vcd {
        builder = builder.vcd(path);
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
    builder
}

/// Low-level handle wrapping a `celox::Simulator`.
///
/// JS holds this as an opaque class; all operations go through methods.
#[napi]
pub struct NativeSimulatorHandle {
    sim: Option<celox::Simulator>,
    layout_json: String,
    events_json: String,
    hierarchy_json: String,
    warnings_json: String,
    stable_size: u32,
    total_size: u32,
}

#[napi]
impl NativeSimulatorHandle {
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
        let source_refs: Vec<(&str, &std::path::Path)> = src_pairs
            .iter()
            .map(|(s, p)| (s.as_str(), p.as_path()))
            .collect();
        let builder = apply_options(celox::Simulator::from_sources(source_refs, &top), &opts);
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
        })
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

    /// Returns analyzer warnings as a JSON array of strings.
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
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        sim.tick_by_id(event_id as usize)
            .map_err(|e| Error::from_reason(format!("{}", e)))
    }

    /// Trigger a clock/event N times in a single NAPI call.
    #[napi]
    pub fn tick_n(&mut self, event_id: u32, count: u32) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        sim.tick_by_id_n(event_id as usize, count)
            .map_err(|e| Error::from_reason(format!("{}", e)))
    }

    /// Evaluate combinational logic.
    #[napi]
    pub fn eval_comb(&mut self) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        sim.eval_comb()
            .map_err(|e| Error::from_reason(format!("{}", e)))
    }

    /// Write VCD dump at the given timestamp.
    #[napi]
    pub fn dump(&mut self, timestamp: f64) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        sim.dump(timestamp as u64);
        Ok(())
    }

    /// Return the simulator's stable memory region as a zero-copy `Uint8Array`.
    /// JS can access `.buffer` to get the underlying `ArrayBuffer`.
    #[napi]
    pub fn shared_memory(&mut self) -> Result<Uint8Array> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        let (ptr, _) = sim.memory_as_mut_ptr();
        let stable_size = sim.stable_region_size();
        Ok(unsafe { Uint8Array::with_external_data(ptr, stable_size, |_, _| {}) })
    }

    /// Invalidate this handle (no-op on the Rust side; drop happens via GC).
    #[napi]
    pub fn dispose(&mut self) {
        self.sim = None;
    }
}

/// Low-level handle wrapping a `celox::Simulation`.
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
        let builder = apply_options(celox::Simulation::from_sources(source_refs, &top), &opts);
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

        let builder = apply_options(
            celox::Simulation::from_sources(source_refs, &top).with_metadata(metadata),
            &opts,
        );
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

    /// Returns analyzer warnings as a JSON array of strings.
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

/// Generate TypeScript type information as JSON for a Veryl project.
///
/// Equivalent to running `celox-gen-ts --json` from the given project directory.
#[napi]
pub fn gen_ts(project_path: String) -> Result<String> {
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
            });
        }
    }

    if paths.is_empty() {
        return Err(Error::from_reason("No Veryl source files found"));
    }

    // Parse and analyze pass 1
    symbol_table::clear();
    attribute_table::clear();

    let analyzer = Analyzer::new(&metadata);
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
            let msgs: Vec<String> = real_errors.iter().map(|e| format!("{e}")).collect();
            return Err(Error::from_reason(format!(
                "Errors in analysis pass 1: {}",
                msgs.join("; ")
            )));
        }
        all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

        parsers.push((path.clone(), parser));
    }

    let results = Analyzer::analyze_post_pass1();
    let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
    if !real_errors.is_empty() {
        let msgs: Vec<String> = real_errors.iter().map(|e| format!("{e}")).collect();
        return Err(Error::from_reason(format!(
            "Errors in post-pass 1 analysis: {}",
            msgs.join("; ")
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
    let mut file_modules: HashMap<String, Vec<String>> = HashMap::new();

    for (i, (path, parser)) in parsers.iter().enumerate() {
        let mut analyzer_context = Context::default();
        let mut ir = Ir::default();
        let results = analyzer.analyze_pass2(
            &path.prj,
            &parser.veryl,
            &mut analyzer_context,
            Some(&mut ir),
        );
        let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
        if !real_errors.is_empty() {
            let msgs: Vec<String> = real_errors.iter().map(|e| format!("{e}")).collect();
            return Err(Error::from_reason(format!(
                "Errors in analysis pass 2: {}",
                msgs.join("; ")
            )));
        }
        all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

        let modules = generate_all(&ir, &source_file_refs);
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
            });
        }
    }

    let results = Analyzer::analyze_post_pass2();
    let real_errors: Vec<_> = results.iter().filter(|e| e.is_error()).collect();
    if !real_errors.is_empty() {
        let msgs: Vec<String> = real_errors.iter().map(|e| format!("{e}")).collect();
        return Err(Error::from_reason(format!(
            "Errors in post-pass 2 analysis: {}",
            msgs.join("; ")
        )));
    }
    all_warnings.extend(results.into_iter().filter(|e| !e.is_error()));

    // Sort for deterministic output
    all_modules.sort_by(|a, b| a.module_name.cmp(&b.module_name));

    let warning_msgs: Vec<String> = all_warnings.iter().map(|w| format!("{w}")).collect();

    let output = JsonOutput {
        project_path: base_path,
        modules: all_modules,
        file_modules,
        warnings: warning_msgs,
    };

    serde_json::to_string(&output)
        .map_err(|e| Error::from_reason(format!("Failed to serialize JSON: {e}")))
}
