//! The simulator handle of native hosts.

use super::*;

/// Low-level handle wrapping the default backend and optional waveform writer.
///
/// JS holds this as an opaque class; all operations go through methods.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub struct NativeSimulatorHandle {
    pub(super) backend: Option<HandleBackend>,
    pub(super) runtime_errors: HashMap<i64, (String, Vec<String>)>,
    pub(super) vcd_writer: Option<celox::WaveformWriter>,
    pub(super) layout_json: String,
    pub(super) events_json: String,
    pub(super) hierarchy_json: String,
    pub(super) warnings_json: String,
    pub(super) stable_size: u32,
    pub(super) total_size: u32,
    pub(super) state_fingerprint: u64,
    pub(super) state_schema: Arc<celox::StateSchema>,
    pub(super) derived_ticks: HandleDerivedTicks,
    pub(super) derived_scratch: celox::DerivedTickScratch,
    pub(super) event_edges: celox::EventEdges,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeSimulatorHandle {
    /// Wrap a simulator built by an external frontend in the standard Celox
    /// N-API handle.
    ///
    /// This is a Rust-only adapter API. A frontend binding can accept its own
    /// artifact type in a `#[napi]` function, lower it with
    /// `celox-frontend-sdk`, build a [`celox::Simulator`], and pass the value
    /// here without serializing an artifact to JSON. Signal metadata is always
    /// derived from the simulator's actual memory layout.
    pub fn from_simulator(simulator: celox::Simulator, vcd_path: Option<&str>) -> Result<Self> {
        Self::from_simulator_with_waveform(
            simulator,
            vcd_path.map(|path| (path, celox::WaveformFormat::Vcd)),
        )
    }

    /// Like [`Self::from_simulator`], for either waveform format.
    pub fn from_simulator_with_waveform(
        simulator: celox::Simulator,
        waveform: Option<(&str, celox::WaveformFormat)>,
    ) -> Result<Self> {
        Self::build_and_cache(simulator, waveform, None)
    }

    /// Build an N-API handle directly from an in-memory frontend artifact.
    ///
    /// Frontend bindings normally expose an artifact-specific function such
    /// as `from_my_artifact` and use this as their final adapter step.
    pub fn from_frontend(
        artifact: celox::FrontendArtifact,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let opts = parse_options(&options)?;
        let builder = apply_options(celox::Simulator::from_frontend(artifact), &opts);
        let simulator = builder
            .build()
            .map_err(|error| Error::from_reason(error.to_string()))?;
        Self::from_simulator_with_waveform(simulator, opts.waveform_arg())
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[napi]
impl NativeSimulatorHandle {
    /// Build a full simulator, extract metadata, cache the compiled code,
    /// and return the handle with the default backend (and optional VcdWriter).
    pub(super) fn build_and_cache(
        sim: celox::Simulator,
        waveform: Option<(&str, celox::WaveformFormat)>,
        cache_key: Option<CacheKey>,
    ) -> Result<Self> {
        let four_state = sim.layout().four_state;
        let warnings_json = format_warnings_json(sim.warnings());
        let signals = sim.named_signals();
        let events = sim.named_events();
        let hierarchy = sim.named_hierarchy();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();
        let state_fingerprint = sim.state_fingerprint();
        let state_schema = sim.state_schema();
        let vcd_descs = sim.build_vcd_descs(four_state);
        let time_precision = sim.program().design.time_precision;
        let runtime_errors = runtime_errors_by_name(sim.program());
        let derived_ticks = sim.derived_tick_plans();
        let event_edges = sim.event_edges();

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
                state_fingerprint,
                state_schema: Arc::clone(&state_schema),
                vcd_descs: vcd_descs.clone(),
                time_precision,
                derived_ticks: Arc::clone(&derived_ticks),
                event_edges: Arc::clone(&event_edges),
            });
            let mut cache = JIT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            cache.insert(key, cached);
        }

        // Create the waveform writer if requested.
        let vcd_writer = if let Some((path, format)) = waveform {
            Some(
                celox::WaveformWriter::new_with_time_precision(
                    path,
                    format,
                    &vcd_descs,
                    time_precision,
                )
                .map_err(|e| {
                    Error::from_reason(format!("Failed to create waveform file: {}", e))
                })?,
            )
        } else {
            None
        };

        // Extract the backend from Simulator (drops runtime metadata which is no longer needed)
        let backend = HandleBackend::Default(sim.into_backend());

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
            state_fingerprint,
            state_schema,
            derived_ticks: HandleDerivedTicks::Default(derived_ticks),
            derived_scratch: Default::default(),
            event_edges,
        })
    }

    /// Extract metadata from a tiered simulator and wrap its backend.
    ///
    /// Mirrors [`Self::build_and_cache`] minus the shared-code cache: the
    /// tiered backend owns its interpreter state and compiles in the
    /// background, so there is no compiled artifact to reuse yet.
    pub(super) fn build_and_cache_tiered(
        mut sim: celox::Simulator<celox::TieredBackend>,
    ) -> Result<Self> {
        // The builder creates the VCD writer itself when recording was
        // requested, selecting a VCD-compatible packed layout in the process;
        // reuse that writer instead of rebuilding descriptors here.
        let vcd_writer = sim.take_waveform_writer();
        let four_state = sim.layout().four_state;
        let warnings_json = format_warnings_json(sim.warnings());
        let signals = sim.named_signals();
        let events = sim.named_events();
        let hierarchy = sim.named_hierarchy();
        let (_, total_size) = sim.memory_as_ptr();
        let stable_size = sim.stable_region_size();
        let state_fingerprint = sim.state_fingerprint();
        let state_schema = sim.state_schema();
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

        // Extract the backend from Simulator (drops runtime metadata which is
        // no longer needed).
        let derived_ticks = HandleDerivedTicks::Tiered(sim.derived_tick_plans());
        let event_edges = sim.event_edges();
        let backend = HandleBackend::Tiered(Box::new(sim.into_backend()));

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
            state_fingerprint,
            state_schema,
            derived_ticks,
            derived_scratch: Default::default(),
            event_edges,
        })
    }

    /// Create a handle from a cached build (shared compiled code + fresh memory).
    pub(super) fn from_cached(
        cached: &CachedBuild,
        waveform: Option<(&str, celox::WaveformFormat)>,
    ) -> Result<Self> {
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        let backend = {
            let mut backend = celox::NativeBackend::from_shared(Arc::clone(&cached.shared_code));
            // Each instance reads the memory files of its initial state (IEEE
            // 1800-2023 21.4). A handle has no runtime event for their
            // messages, which are traced.
            for report in backend.load_initial_memory_files() {
                if report.severity == celox::Severity::Warning {
                    tracing::warn!("{}", report.message);
                } else {
                    tracing::error!("{}", report.message);
                }
            }
            HandleBackend::Default(backend)
        };
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        let backend = HandleBackend::Default(celox::JitBackend::from_shared(Arc::clone(
            &cached.shared_code,
        )));
        let vcd_writer = if let Some((path, format)) = waveform {
            Some(
                celox::WaveformWriter::new_with_time_precision(
                    path,
                    format,
                    &cached.vcd_descs,
                    cached.time_precision,
                )
                .map_err(|e| {
                    Error::from_reason(format!("Failed to create waveform file: {}", e))
                })?,
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
            state_fingerprint: cached.state_fingerprint,
            state_schema: Arc::clone(&cached.state_schema),
            derived_ticks: HandleDerivedTicks::Default(Arc::clone(&cached.derived_ticks)),
            derived_scratch: Default::default(),
            event_edges: Arc::clone(&cached.event_edges),
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
                return Self::from_cached(cached, opts.waveform_arg());
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

        Self::build_and_cache(sim, opts.waveform_arg(), Some(cache_key))
    }

    /// Create a new tiered simulator from Veryl source code.
    ///
    /// Execution starts on the interpreter immediately while the host's
    /// default compiled tier (native where available) is prepared on a
    /// background thread. The first safe point after compilation completes
    /// adopts the compiled code; observe progress through
    /// [`Self::tier_compiled`](Self::tier_compiled). Tiered builds bypass the
    /// JIT cache because the interpreter already removes startup latency.
    #[napi(factory)]
    pub fn new_tiered(
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
            .build_tiered()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        Self::build_and_cache_tiered(sim)
    }

    /// Create a new tiered simulator from a Veryl project directory.
    ///
    /// Tiered counterpart of [`Self::from_project`]: searches upward from
    /// `project_path` for `Veryl.toml`, starts on the interpreter
    /// immediately, and promotes to the host's default compiled tier in the
    /// background. Bypasses the JIT cache for the same reason as
    /// [`Self::new_tiered`].
    #[napi(factory)]
    pub fn new_tiered_from_project(
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
            celox::Simulator::from_sources(source_refs, &top).with_metadata(metadata),
            &opts,
        );
        let sim = builder
            .build_tiered()
            .map_err(|e| Error::from_reason(format!("{}", e)))?;

        Self::build_and_cache_tiered(sim)
    }

    /// Create a simulator from a versioned external-frontend artifact.
    #[napi(factory)]
    pub fn from_frontend_artifact(
        artifact_json: String,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let artifact = celox::FrontendArtifact::from_json(&artifact_json)
            .map_err(|error| Error::from_reason(error.to_string()))?;
        Self::from_frontend(artifact, options)
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
        let mut opts = parse_options(&options)?;
        let (mut sources, metadata, celox_cfg) = load_project_sources(&project_path)?;
        opts.common.diagnostic_policy = celox_cfg.diagnostics.clone();
        append_extra_source(&mut sources, &opts.extra_source);

        let cache_key = build_cache_key(&sources, &top, &opts, Some(&metadata));

        {
            let cache = JIT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cache.get(&cache_key) {
                return Self::from_cached(cached, opts.waveform_arg());
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

        Self::build_and_cache(sim, opts.waveform_arg(), Some(cache_key))
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

    /// Whether this handle drives the tiered backend.
    #[napi(getter)]
    pub fn is_tiered(&self) -> bool {
        matches!(self.backend, Some(HandleBackend::Tiered(_)))
    }

    /// Whether the tiered backend has adopted its compiled tier.
    ///
    /// `None` when the handle is not tiered; `false` while background
    /// compilation is still running or after it failed (the interpreter then
    /// remains the permanent tier).
    #[napi(getter)]
    pub fn tier_compiled(&self) -> Option<bool> {
        match &self.backend {
            Some(HandleBackend::Tiered(backend)) => Some(backend.is_compiled()),
            _ => None,
        }
    }

    /// Trigger a clock/event by its numeric ID.
    #[napi]
    pub fn tick(&mut self, event_id: u32) -> Result<()> {
        let runtime_errors = self.runtime_errors.clone();
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        validate_event_id(event_id, b.event_count())?;
        b.tick(
            event_id as usize,
            &self.derived_ticks,
            &self.event_edges,
            &mut self.derived_scratch,
        )
        .map_err(|e| napi_runtime_error(&runtime_errors, e))
    }

    /// Move event signals through coincident edges and run every domain
    /// sensitive to them: `rising[i]` gives the direction of `event_ids[i]`.
    /// See `Simulator::edges`. `dirty` says whether combinational logic has
    /// pending changes (default: it has, so it settles first). Returns whether
    /// combinational logic is settled afterwards: it is not when a signal that
    /// combinational logic reads moved but no domain ran, or when it was
    /// dirty and nothing ran.
    #[napi]
    pub fn edges(
        &mut self,
        event_ids: Vec<u32>,
        rising: Vec<bool>,
        dirty: Option<bool>,
    ) -> Result<bool> {
        if event_ids.len() != rising.len() {
            return Err(Error::from_reason(
                "edges needs one direction per event".to_string(),
            ));
        }
        let runtime_errors = self.runtime_errors.clone();
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        let mut moves = Vec::with_capacity(event_ids.len());
        for (&event_id, &rising) in event_ids.iter().zip(&rising) {
            validate_event_id(event_id, b.event_count())?;
            let edge = if rising {
                celox::Edge::Rising
            } else {
                celox::Edge::Falling
            };
            moves.push((event_id as usize, edge));
        }
        b.edges(
            &moves,
            &self.derived_ticks,
            &self.event_edges,
            &mut self.derived_scratch,
            dirty.unwrap_or(true),
        )
        .map_err(|e| napi_runtime_error(&runtime_errors, e))
    }

    /// Whether the event's active edge is a rising one: a positive-edge
    /// clock or an active-high reset.
    #[napi]
    pub fn active_edge_is_rising(&self, event_id: u32) -> Result<bool> {
        let b = self
            .backend
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        validate_event_id(event_id, b.event_count())?;
        Ok(self.event_edges[event_id as usize].active_edge() == celox::Edge::Rising)
    }

    /// Trigger a clock/event N times in a single NAPI call.
    #[napi]
    pub fn tick_n(&mut self, event_id: u32, count: u32) -> Result<()> {
        let runtime_errors = self.runtime_errors.clone();
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        // Validate even when `count` is zero so invalid raw IDs never receive
        // call-shape-dependent treatment at the N-API boundary.
        validate_event_id(event_id, b.event_count())?;
        for _ in 0..count {
            b.tick(
                event_id as usize,
                &self.derived_ticks,
                &self.event_edges,
                &mut self.derived_scratch,
            )
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

    /// Write a waveform dump at the given timestamp.
    #[napi]
    pub fn dump(&mut self, timestamp: f64) -> Result<()> {
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        if let Some(ref mut writer) = self.vcd_writer {
            match b {
                HandleBackend::Default(backend) => {
                    writer.dump_backend(timestamp as u64, backend, &[])
                }
                HandleBackend::Tiered(backend) => {
                    writer.dump_backend(timestamp as u64, backend.as_mut(), &[])
                }
            }
            .map_err(|e| Error::from_reason(format!("waveform write error: {}", e)))?;
        }
        Ok(())
    }

    /// Finish the current VCD file and continue the waveform in a new file,
    /// whose timestamps start over.
    #[napi]
    pub fn switch_vcd(&mut self, path: String) -> Result<()> {
        let writer = self.vcd_writer.as_mut().ok_or_else(|| {
            Error::from_reason("waveform output was not enabled when the simulator was created")
        })?;
        writer
            .restart(&path)
            .map_err(|e| Error::from_reason(format!("failed to start {path}: {e}")))
    }

    /// Save the design state.
    #[napi]
    pub fn checkpoint(&self) -> Result<NativeSimulatorCheckpoint> {
        let b = self
            .backend
            .as_ref()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        Ok(NativeSimulatorCheckpoint {
            image: b.capture_state(self.state_fingerprint),
        })
    }

    /// Return to the state saved in `checkpoint` and settle combinational
    /// logic.
    #[napi]
    pub fn restore(&mut self, checkpoint: &NativeSimulatorCheckpoint) -> Result<()> {
        let runtime_errors = self.runtime_errors.clone();
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        b.restore_state(&checkpoint.image, self.state_fingerprint)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        if let Some(writer) = &mut self.vcd_writer {
            writer.rescan();
        }
        b.eval_comb()
            .map_err(|e| napi_runtime_error(&runtime_errors, e))
    }

    /// Save the value of every state object, by path, as state file bytes.
    #[napi]
    pub fn save_state(&mut self) -> Result<Buffer> {
        let runtime_errors = self.runtime_errors.clone();
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        b.eval_comb()
            .map_err(|e| napi_runtime_error(&runtime_errors, e))?;
        let file = b
            .save_state_file(&self.state_schema)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        state_file_bytes(&file)
    }

    /// Load state file bytes, matching objects by path, and settle
    /// combinational logic.
    #[napi]
    pub fn load_state(&mut self, bytes: Uint8Array) -> Result<()> {
        let file = parse_state_file(&bytes)?;
        let runtime_errors = self.runtime_errors.clone();
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        b.load_state_file(&self.state_schema, &file)
            .map_err(|mismatch| {
                Error::from_reason(celox::StateError::Mismatch(mismatch).to_string())
            })?;
        if let Some(writer) = &mut self.vcd_writer {
            writer.rescan();
        }
        b.eval_comb()
            .map_err(|e| napi_runtime_error(&runtime_errors, e))
    }

    /// Return the simulator's stable memory region as a zero-copy `Uint8Array`.
    /// JS can access `.buffer` to get the underlying `ArrayBuffer`.
    #[napi]
    pub fn shared_memory(&mut self) -> Result<Uint8Array> {
        let b = self
            .backend
            .as_mut()
            .ok_or_else(|| Error::from_reason("Simulator has been disposed"))?;
        let owner = b.memory_owner().ok_or_else(|| {
            Error::from_reason("Simulator backend does not expose owned shared memory")
        })?;
        let (ptr, _) = b.memory_as_mut_ptr();
        let stable_size = b.stable_region_size();
        Ok(unsafe { Uint8Array::with_external_data(ptr, stable_size, move |_, _| drop(owner)) })
    }

    /// Invalidate this handle and release its simulator resources.
    #[napi]
    pub fn dispose(&mut self) -> Result<()> {
        self.backend = None;
        if let Some(mut writer) = self.vcd_writer.take() {
            writer
                .flush()
                .map_err(|e| Error::from_reason(format!("waveform flush error: {e}")))?;
        }
        Ok(())
    }
}
