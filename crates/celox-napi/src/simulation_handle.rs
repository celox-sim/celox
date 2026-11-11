//! The timed simulation handle and checkpoints.

use super::*;

/// Low-level handle wrapping a `celox::Simulation`.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub struct NativeSimulationHandle {
    pub(super) sim: Option<celox::Simulation>,
    pub(super) layout_json: String,
    pub(super) events_json: String,
    pub(super) hierarchy_json: String,
    pub(super) warnings_json: String,
    pub(super) stable_size: u32,
    pub(super) total_size: u32,
    /// Default `maxSteps` for `waitUntil` / `waitForCycles`, sourced from
    /// `[simulation] max_steps` in `celox.toml`. `None` when not set.
    pub(super) default_max_steps: Option<u32>,
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn state_file_bytes(file: &celox::StateFile) -> Result<Buffer> {
    let mut bytes = Vec::new();
    file.write_to(&mut bytes)
        .map_err(|e| Error::from_reason(format!("failed to encode state: {e}")))?;
    Ok(bytes.into())
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn parse_state_file(bytes: &[u8]) -> Result<celox::StateFile> {
    celox::StateFile::read_from(bytes).map_err(|e| Error::from_reason(e.to_string()))
}

/// A periodic clock of a [`NativeSimulationHandle`].
#[cfg(not(target_arch = "wasm32"))]
#[napi(object)]
pub struct NapiClockPeriod {
    pub event_id: u32,
    pub period: f64,
}

/// Saved state of a [`NativeSimulatorHandle`].
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub struct NativeSimulatorCheckpoint {
    pub(super) image: celox::StateImage,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi]
impl NativeSimulatorCheckpoint {
    /// Size of the saved design state in bytes.
    #[napi(getter)]
    pub fn state_size(&self) -> u32 {
        self.image.len() as u32
    }
}

/// Saved state of a [`NativeSimulationHandle`].
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub struct NativeSimulationCheckpoint {
    pub(super) inner: celox::SimulationCheckpoint,
}

#[cfg(not(target_arch = "wasm32"))]
#[napi]
impl NativeSimulationCheckpoint {
    /// Simulation time at which the checkpoint was taken.
    #[napi(getter)]
    pub fn time(&self) -> f64 {
        self.inner.time() as f64
    }

    /// Size of the saved design state in bytes.
    #[napi(getter)]
    pub fn state_size(&self) -> u32 {
        self.inner.state_size() as u32
    }
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

    /// Create a timed simulation from a versioned external-frontend artifact.
    #[napi(factory)]
    pub fn from_frontend_artifact(
        artifact_json: String,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let opts = parse_options(&options)?;
        let artifact = celox::FrontendArtifact::from_json(&artifact_json)
            .map_err(|error| Error::from_reason(error.to_string()))?;
        let builder = apply_options(celox::Simulation::from_frontend(artifact), &opts);
        let sim = builder
            .build()
            .map_err(|error| Error::from_reason(error.to_string()))?;

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
            .map_err(|error| Error::from_reason(format!("Failed to serialize layout: {error}")))?;
        let events_json = serde_json::to_string(&event_map)
            .map_err(|error| Error::from_reason(format!("Failed to serialize events: {error}")))?;
        let hierarchy_json = serde_json::to_string(&hierarchy_node).map_err(|error| {
            Error::from_reason(format!("Failed to serialize hierarchy: {error}"))
        })?;

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
        let mut opts = parse_options(&options)?;
        let (mut sources, metadata, celox_cfg) = load_project_sources(&project_path)?;
        opts.common.diagnostic_policy = celox_cfg.diagnostics.clone();
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
        sim.try_add_clock_by_id(event_id, period as u64, initial_delay as u64)
            .map_err(|e| Error::from_reason(e.to_string()))
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
            .map_err(|e| Error::from_reason(format!("{}", e)))?;
        check_failure(sim)
    }

    /// Advance until `end_time` taking at most `max_steps` steps. Returns
    /// false, at the time of the last step, when the steps ran out first.
    #[napi]
    pub fn run_until_bounded(&mut self, end_time: f64, max_steps: f64) -> Result<bool> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        let reached = sim
            .run_until_bounded(end_time as u64, max_steps as u64)
            .map_err(|e| Error::from_reason(format!("{}", e)))?;
        check_failure(sim)?;
        Ok(reached)
    }

    /// Take the runtime events the design emitted since the last call, in
    /// emission order.
    #[napi]
    pub fn drain_runtime_events(&mut self) -> Result<Vec<NapiRuntimeEvent>> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        Ok(sim
            .drain_runtime_events()
            .into_iter()
            .map(napi_runtime_event)
            .collect())
    }

    /// Make runs and steps fail after the time step of a report of at least
    /// `severity` (`"info"`, `"warning"`, `"error"` or `"fatal"`); `null`
    /// fails only at `$fatal`.
    #[napi]
    pub fn set_fail_on(&mut self, severity: Option<String>) -> Result<()> {
        let severity = severity.as_deref().map(parse_severity).transpose()?;
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.set_fail_on(severity);
        Ok(())
    }

    /// Report that the host wrote the state through the shared memory: the
    /// next step settles the current time when a process may see the writes
    /// or an event signal changed.
    #[napi]
    pub fn note_host_writes(&mut self) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.note_raw_writes();
        Ok(())
    }

    /// Advance to the next event. Returns the new time, or null if no events.
    #[napi]
    pub fn step(&mut self) -> Result<Option<f64>> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        let time = sim
            .step()
            .map(|opt| opt.map(|t| t as f64))
            .map_err(|e| Error::from_reason(format!("{}", e)))?;
        check_failure(sim)?;
        Ok(time)
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

    /// Returns whether a process of the design requested the end of the
    /// simulation.
    #[napi]
    pub fn is_finished(&self) -> Result<bool> {
        let sim = self
            .sim
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        Ok(sim.is_finished())
    }

    /// Returns whether the last step or run returned early because a process
    /// of the design executed `$stop`. The next step continues it.
    #[napi]
    pub fn is_stopped(&self) -> Result<bool> {
        let sim = self
            .sim
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        Ok(sim.is_stopped())
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

    /// Write a waveform dump at the given timestamp.
    #[napi]
    pub fn dump(&mut self, timestamp: f64) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.try_dump(timestamp as u64)
            .map_err(|e| Error::from_reason(format!("VCD write error: {e}")))
    }

    /// Finish the current VCD file and continue the waveform in a new file,
    /// whose timestamps start over.
    #[napi]
    pub fn switch_vcd(&mut self, path: String) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.switch_vcd(&path)
            .map_err(|e| Error::from_reason(format!("failed to start {path}: {e}")))
    }

    /// Save the design state, simulation time, clocks and pending events.
    #[napi]
    pub fn checkpoint(&self) -> Result<NativeSimulationCheckpoint> {
        let sim = self
            .sim
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        let inner = sim
            .checkpoint()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NativeSimulationCheckpoint { inner })
    }

    /// Return to the state saved in `checkpoint`.
    #[napi]
    pub fn restore(&mut self, checkpoint: &NativeSimulationCheckpoint) -> Result<()> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.restore(&checkpoint.inner)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        // JS reads outputs straight from memory, so settle them now.
        sim.eval_comb()
            .map_err(|e| Error::from_reason(format!("{}", e)))
    }

    /// Periodic clocks currently registered, including those restored from a
    /// checkpoint or state file.
    #[napi]
    pub fn clock_periods(&self) -> Result<Vec<NapiClockPeriod>> {
        let sim = self
            .sim
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        Ok(sim
            .clock_periods()
            .into_iter()
            .map(|(event_id, period)| NapiClockPeriod {
                event_id: event_id as u32,
                period: period as f64,
            })
            .collect())
    }

    /// Save the design state, time, clocks and pending events, by name, as
    /// state file bytes.
    #[napi]
    pub fn save_state(&mut self) -> Result<Buffer> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        // JS writes inputs straight to memory without marking the simulation
        // dirty, so settle combinational logic before saving it.
        sim.eval_comb()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;
        let file = sim
            .save_state()
            .map_err(|e| Error::from_reason(e.to_string()))?;
        state_file_bytes(&file)
    }

    /// Load state file bytes saved from a simulation. Combinational logic is
    /// settled as part of loading.
    #[napi]
    pub fn load_state(&mut self, bytes: Uint8Array) -> Result<()> {
        let file = parse_state_file(&bytes)?;
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        sim.load_state(&file)
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Return the simulation's stable memory region as a zero-copy `Uint8Array`.
    /// JS can access `.buffer` to get the underlying `ArrayBuffer`.
    #[napi]
    pub fn shared_memory(&mut self) -> Result<Uint8Array> {
        let sim = self
            .sim
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulation has been disposed"))?;
        let owner = sim.memory_owner().ok_or_else(|| {
            Error::from_reason("Simulation backend does not expose owned shared memory")
        })?;
        let (ptr, _) = sim.memory_as_mut_ptr();
        let stable_size = sim.stable_region_size();
        Ok(unsafe { Uint8Array::with_external_data(ptr, stable_size, move |_, _| drop(owner)) })
    }

    /// Invalidate this handle.
    #[napi]
    pub fn dispose(&mut self) -> Result<()> {
        if let Some(mut sim) = self.sim.take() {
            sim.flush_vcd()
                .map_err(|e| Error::from_reason(format!("waveform flush error: {e}")))?;
        }
        Ok(())
    }
}
