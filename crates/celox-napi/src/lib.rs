mod layout;

use std::collections::HashMap;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use veryl_analyzer::{Analyzer, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::Parser;

use layout::{build_event_map, build_signal_layout};

/// Options for creating a simulator/simulation handle.
#[napi(object)]
pub struct NapiOptions {
    pub four_state: Option<bool>,
    pub vcd: Option<String>,
}

/// Parsed builder options from NapiOptions.
struct ParsedOptions {
    four_state: bool,
    vcd: Option<String>,
}

/// Helper to extract the builder config from NapiOptions.
fn parse_options(options: &Option<NapiOptions>) -> ParsedOptions {
    match options.as_ref() {
        Some(o) => ParsedOptions {
            four_state: o.four_state.unwrap_or(false),
            vcd: o.vcd.clone(),
        },
        None => ParsedOptions {
            four_state: false,
            vcd: None,
        },
    }
}

/// Load a Veryl project's source files and metadata from a directory.
///
/// Searches upward from `project_path` for `Veryl.toml`, gathers all `.veryl`
/// source files, and returns the concatenated source along with the project metadata.
fn load_project_source(project_path: &str) -> Result<(String, Metadata)> {
    let toml_path = Metadata::search_from(project_path)
        .map_err(|e| Error::from_reason(format!("Could not find Veryl.toml: {e}")))?;
    let mut metadata = Metadata::load(&toml_path)
        .map_err(|e| Error::from_reason(format!("Failed to load Veryl.toml: {e}")))?;
    let paths = metadata
        .paths::<&str>(&[], false, false)
        .map_err(|e| Error::from_reason(format!("Failed to gather sources: {e}")))?;
    let mut source = String::new();
    for p in &paths {
        let content = std::fs::read_to_string(&p.src)
            .map_err(|e| Error::from_reason(format!("{}: {e}", p.src.display())))?;
        source.push_str(&content);
        source.push('\n');
    }
    Ok((source, metadata))
}

/// Low-level handle wrapping a `celox::Simulator`.
///
/// JS holds this as an opaque class; all operations go through methods.
#[napi]
pub struct NativeSimulatorHandle {
    sim: Option<celox::Simulator>,
    layout_json: String,
    events_json: String,
    stable_size: u32,
    total_size: u32,
}

#[napi]
impl NativeSimulatorHandle {
    /// Create a new simulator from Veryl source code.
    #[napi(constructor)]
    pub fn new(code: String, top: String, options: Option<NapiOptions>) -> Result<Self> {
        let opts = parse_options(&options);
        let mut builder = celox::Simulator::builder(&code, &top)
            .four_state(opts.four_state);
        if let Some(path) = &opts.vcd {
            builder = builder.vcd(path);
        }
        let sim = builder
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let signals = sim.named_signals();
        let events = sim.named_events();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();

        let layout_map = build_signal_layout(&signals, opts.four_state);
        let event_map = build_event_map(&events);

        let layout_json = serde_json::to_string(&layout_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize layout: {}", e)))?;
        let events_json = serde_json::to_string(&event_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize events: {}", e)))?;

        Ok(Self {
            sim: Some(sim),
            layout_json,
            events_json,
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
    pub fn from_project(project_path: String, top: String, options: Option<NapiOptions>) -> Result<Self> {
        let opts = parse_options(&options);
        let (source, metadata) = load_project_source(&project_path)?;

        let mut builder = celox::Simulator::builder(&source, &top)
            .with_metadata(metadata)
            .four_state(opts.four_state);
        if let Some(path) = &opts.vcd {
            builder = builder.vcd(path);
        }
        let sim = builder
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let signals = sim.named_signals();
        let events = sim.named_events();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();

        let layout_map = build_signal_layout(&signals, opts.four_state);
        let event_map = build_event_map(&events);

        let layout_json = serde_json::to_string(&layout_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize layout: {}", e)))?;
        let events_json = serde_json::to_string(&event_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize events: {}", e)))?;

        Ok(Self {
            sim: Some(sim),
            layout_json,
            events_json,
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
    stable_size: u32,
    total_size: u32,
}

#[napi]
impl NativeSimulationHandle {
    /// Create a new timed simulation from Veryl source code.
    #[napi(constructor)]
    pub fn new(code: String, top: String, options: Option<NapiOptions>) -> Result<Self> {
        let opts = parse_options(&options);
        let mut builder = celox::Simulation::builder(&code, &top)
            .four_state(opts.four_state);
        if let Some(path) = &opts.vcd {
            builder = builder.vcd(path);
        }
        let sim = builder
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let signals = sim.named_signals();
        let events = sim.named_events();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();

        let layout_map = build_signal_layout(&signals, opts.four_state);
        let event_map = build_event_map(&events);

        let layout_json = serde_json::to_string(&layout_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize layout: {}", e)))?;
        let events_json = serde_json::to_string(&event_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize events: {}", e)))?;

        Ok(Self {
            sim: Some(sim),
            layout_json,
            events_json,
            stable_size: stable_size as u32,
            total_size: total_size as u32,
        })
    }

    /// Create a new timed simulation from a Veryl project directory.
    #[napi(factory)]
    pub fn from_project(project_path: String, top: String, options: Option<NapiOptions>) -> Result<Self> {
        let opts = parse_options(&options);
        let (source, metadata) = load_project_source(&project_path)?;

        let mut builder = celox::Simulation::builder(&source, &top)
            .with_metadata(metadata)
            .four_state(opts.four_state);
        if let Some(path) = &opts.vcd {
            builder = builder.vcd(path);
        }
        let sim = builder
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let signals = sim.named_signals();
        let events = sim.named_events();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();

        let layout_map = build_signal_layout(&signals, opts.four_state);
        let event_map = build_event_map(&events);

        let layout_json = serde_json::to_string(&layout_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize layout: {}", e)))?;
        let events_json = serde_json::to_string(&event_map)
            .map_err(|e| Error::from_reason(format!("Failed to serialize events: {}", e)))?;

        Ok(Self {
            sim: Some(sim),
            layout_json,
            events_json,
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

    let paths = metadata
        .paths::<std::path::PathBuf>(&[], true, true)
        .map_err(|e| Error::from_reason(format!("Failed to gather sources: {e}")))?;
    if paths.is_empty() {
        return Err(Error::from_reason("No Veryl source files found"));
    }

    // Parse and analyze pass 1
    symbol_table::clear();
    attribute_table::clear();

    let analyzer = Analyzer::new(&metadata);
    let mut parsers = Vec::new();

    for path in &paths {
        let input = std::fs::read_to_string(&path.src)
            .map_err(|e| Error::from_reason(format!("{}: {e}", path.src.display())))?;
        let parser = Parser::parse(&input, &path.src)
            .map_err(|e| Error::from_reason(format!("Parse error: {e}")))?;

        let errors = analyzer.analyze_pass1(&path.prj, &parser.veryl);
        if !errors.is_empty() {
            let msgs: Vec<String> = errors.iter().map(|e| format!("{e}")).collect();
            return Err(Error::from_reason(format!(
                "Errors in analysis pass 1: {}",
                msgs.join("; ")
            )));
        }

        parsers.push((path.clone(), parser));
    }

    let errors = Analyzer::analyze_post_pass1();
    if !errors.is_empty() {
        let msgs: Vec<String> = errors.iter().map(|e| format!("{e}")).collect();
        return Err(Error::from_reason(format!(
            "Errors in post-pass 1 analysis: {}",
            msgs.join("; ")
        )));
    }

    // Pass 2: per-file IR → generate
    let mut all_modules = Vec::new();
    let mut file_modules: HashMap<String, Vec<String>> = HashMap::new();

    for (path, parser) in &parsers {
        let mut analyzer_context = Context::default();
        let mut ir = Ir::default();
        let _errors = analyzer.analyze_pass2(
            &path.prj,
            &parser.veryl,
            &mut analyzer_context,
            Some(&mut ir),
        );

        let modules = generate_all(&ir);
        // On Windows, metadata.paths() may return extended-length paths
        // (\\?\...) while base_path does not have the prefix, so strip it
        // before computing the relative source_file key.
        let src_normalized = path.src.to_string_lossy().replace(r"\\?\", "").replace('\\', "/");
        let base_normalized = base_path.replace('\\', "/");
        let source_file = src_normalized
            .strip_prefix(&base_normalized)
            .unwrap_or(&src_normalized)
            .trim_start_matches('/')
            .to_string();

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
            });
        }
    }

    let _errors = Analyzer::analyze_post_pass2();

    // Sort for deterministic output
    all_modules.sort_by(|a, b| a.module_name.cmp(&b.module_name));

    let output = JsonOutput {
        project_path: base_path,
        modules: all_modules,
        file_modules,
    };

    serde_json::to_string(&output)
        .map_err(|e| Error::from_reason(format!("Failed to serialize JSON: {e}")))
}
