mod layout;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use veryl_metadata::Metadata;

use layout::{build_event_map, build_signal_layout};

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
    pub fn new(code: String, top: String) -> Result<Self> {
        let sim = celox::Simulator::builder(&code, &top)
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let signals = sim.named_signals();
        let events = sim.named_events();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();

        let layout_map = build_signal_layout(&signals);
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
    pub fn from_project(project_path: String, top: String) -> Result<Self> {
        let (source, metadata) = load_project_source(&project_path)?;

        let sim = celox::Simulator::builder(&source, &top)
            .with_metadata(metadata)
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let signals = sim.named_signals();
        let events = sim.named_events();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();

        let layout_map = build_signal_layout(&signals);
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

    /// Read signal bytes from memory. Returns a copy of the stable region.
    #[napi]
    pub fn read_memory(&self) -> Result<Buffer> {
        let sim = self
            .sim
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        let (ptr, _) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();
        let bytes = unsafe { std::slice::from_raw_parts(ptr, stable_size) };
        Ok(Buffer::from(bytes.to_vec()))
    }

    /// Write bytes into the stable region of memory.
    #[napi]
    pub fn write_memory(&mut self, data: Buffer, offset: u32) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        let (ptr, _) = sim.memory_as_mut_ptr();
        let stable_size = sim.stable_region_size();
        let offset = offset as usize;
        if offset + data.len() > stable_size {
            return Err(Error::from_reason("Write exceeds stable region"));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.add(offset), data.len());
        }
        Ok(())
    }

    /// Tick with synchronised memory: write input buffer → tick → return output buffer.
    #[napi]
    pub fn tick_synced(&mut self, event_id: u32, input: Buffer) -> Result<Buffer> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        let (ptr, _) = sim.memory_as_mut_ptr();
        let stable_size = sim.stable_region_size();
        let copy_len = stable_size.min(input.len());
        unsafe {
            std::ptr::copy_nonoverlapping(input.as_ptr(), ptr, copy_len);
        }
        sim.tick_by_id(event_id as usize)
            .map_err(|e| Error::from_reason(format!("{}", e)))?;
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, stable_size) };
        Ok(Buffer::from(bytes.to_vec()))
    }

    /// Eval combinational with synchronised memory.
    #[napi]
    pub fn eval_comb_synced(&mut self, input: Buffer) -> Result<Buffer> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        let (ptr, _) = sim.memory_as_mut_ptr();
        let stable_size = sim.stable_region_size();
        let copy_len = stable_size.min(input.len());
        unsafe {
            std::ptr::copy_nonoverlapping(input.as_ptr(), ptr, copy_len);
        }
        sim.eval_comb()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, stable_size) };
        Ok(Buffer::from(bytes.to_vec()))
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
    pub fn new(code: String, top: String) -> Result<Self> {
        let sim = celox::Simulation::builder(&code, &top)
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let signals = sim.named_signals();
        let events = sim.named_events();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();

        let layout_map = build_signal_layout(&signals);
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
    pub fn from_project(project_path: String, top: String) -> Result<Self> {
        let (source, metadata) = load_project_source(&project_path)?;

        let sim = celox::Simulation::builder(&source, &top)
            .with_metadata(metadata)
            .build()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let signals = sim.named_signals();
        let events = sim.named_events();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();

        let layout_map = build_signal_layout(&signals);
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

    /// Read signal bytes from memory. Returns a copy of the stable region.
    #[napi]
    pub fn read_memory(&self) -> Result<Buffer> {
        let sim = self
            .sim
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        let (ptr, _) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();
        let bytes = unsafe { std::slice::from_raw_parts(ptr, stable_size) };
        Ok(Buffer::from(bytes.to_vec()))
    }

    /// Write bytes into the stable region of memory.
    #[napi]
    pub fn write_memory(&mut self, data: Buffer, offset: u32) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        let (ptr, _) = sim.memory_as_mut_ptr();
        let stable_size = sim.stable_region_size();
        let offset = offset as usize;
        if offset + data.len() > stable_size {
            return Err(Error::from_reason("Write exceeds stable region"));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr.add(offset), data.len());
        }
        Ok(())
    }

    /// Run-until with synchronised memory.
    #[napi]
    pub fn run_until_synced(&mut self, end_time: f64, input: Buffer) -> Result<Buffer> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        let (ptr, _) = sim.memory_as_mut_ptr();
        let stable_size = sim.stable_region_size();
        let copy_len = stable_size.min(input.len());
        unsafe {
            std::ptr::copy_nonoverlapping(input.as_ptr(), ptr, copy_len);
        }
        sim.run_until(end_time as u64)
            .map_err(|e| Error::from_reason(format!("{}", e)))?;
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, stable_size) };
        Ok(Buffer::from(bytes.to_vec()))
    }

    /// Step with synchronised memory.
    #[napi]
    pub fn step_synced(&mut self, input: Buffer) -> Result<StepSyncedResult> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        let (ptr, _) = sim.memory_as_mut_ptr();
        let stable_size = sim.stable_region_size();
        let copy_len = stable_size.min(input.len());
        unsafe {
            std::ptr::copy_nonoverlapping(input.as_ptr(), ptr, copy_len);
        }
        let time = sim
            .step()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, stable_size) };
        Ok(StepSyncedResult {
            time: time.map(|t| t as f64),
            buffer: Buffer::from(bytes.to_vec()),
        })
    }

    /// Eval combinational with synchronised memory.
    #[napi]
    pub fn eval_comb_synced(&mut self, input: Buffer) -> Result<Buffer> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        let (ptr, _) = sim.memory_as_mut_ptr();
        let stable_size = sim.stable_region_size();
        let copy_len = stable_size.min(input.len());
        unsafe {
            std::ptr::copy_nonoverlapping(input.as_ptr(), ptr, copy_len);
        }
        sim.eval_comb()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, stable_size) };
        Ok(Buffer::from(bytes.to_vec()))
    }

    /// Invalidate this handle.
    #[napi]
    pub fn dispose(&mut self) {
        self.sim = None;
    }
}

/// Result from `step_synced` — contains both the time and the updated buffer.
#[napi(object)]
pub struct StepSyncedResult {
    pub time: Option<f64>,
    pub buffer: Buffer,
}
