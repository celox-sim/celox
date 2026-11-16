//! The simulator handle of WebAssembly hosts.

use super::*;

// ---------------------------------------------------------------------------
//  WASM32 NativeSimulatorHandle — compiles Veryl to WASM bytecode
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[napi]
pub struct NativeSimulatorHandle {
    pub(super) program: celox::LaidOutProgram,
    pub(super) four_state: bool,
    pub(super) layout_json: String,
    pub(super) events_json: String,
    pub(super) hierarchy_json: String,
    pub(super) warnings_json: String,
    pub(super) stable_size: u32,
    pub(super) total_size: u32,
}

#[cfg(target_arch = "wasm32")]
impl NativeSimulatorHandle {
    /// Build an N-API/WASI handle directly from an in-memory frontend
    /// artifact.
    ///
    /// Frontend bindings can accept their own artifact type, lower it with
    /// `celox-frontend-sdk`, and call this Rust-only adapter API without a JSON
    /// serialization boundary.
    pub fn from_frontend(
        artifact: celox::FrontendArtifact,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let opts = parse_options_common(&options)?;
        let trace_opts = celox::TraceOptions::default();
        let (program, warnings) = celox::compile_frontend_to_sir(
            &artifact,
            &opts.false_loops,
            &opts.true_loops,
            opts.four_state,
            &trace_opts,
            None,
            &opts.optimize_options,
        )
        .map_err(|error| Error::from_reason(error.to_string()))?;

        let laid_out = program
            .into_laid_out_with_mode(opts.four_state, celox::MemoryLayoutMode::ElementStrided);
        let layout = laid_out.layout();
        let layout_json = Self::build_layout_json(&laid_out, layout, opts.four_state);
        let events_json = Self::build_events_json(&laid_out);
        let warnings_json = format_warnings_json(&warnings);
        let stable_size = layout.total_size as u32;
        let total_size = layout.merged_total_size as u32;

        Ok(Self {
            program: laid_out,
            four_state: opts.four_state,
            layout_json,
            events_json,
            hierarchy_json: "{}".to_string(),
            warnings_json,
            stable_size,
            total_size,
        })
    }
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
        let (program, warnings) = celox::compile_to_sir_with_diagnostic_policy(
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
            &opts
                .diagnostic_policy
                .clone()
                .with_inferred_latches(opts.allow_inferred_latches),
            &Default::default(),
        )
        .map_err(|e| Error::from_reason(format!("{}", e)))?;

        let laid_out = program
            .into_laid_out_with_mode(opts.four_state, celox::MemoryLayoutMode::ElementStrided);
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

    /// Create a WASM-oriented handle from a versioned external-frontend artifact.
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
    #[napi(factory)]
    pub fn from_project(
        project_path: String,
        top: String,
        options: Option<NapiOptions>,
    ) -> Result<Self> {
        let mut opts = parse_options_common(&options)?;
        let (mut sources, metadata, celox_cfg) = load_project_sources(&project_path)?;
        opts.diagnostic_policy = celox_cfg.diagnostics.clone();
        append_extra_source(&mut sources, &opts.extra_source);

        let source_refs: Vec<(&str, &std::path::Path)> = sources
            .iter()
            .map(|(s, p)| (s.as_str(), p.as_path()))
            .collect();

        let trace_opts = celox::TraceOptions::default();
        let (program, warnings) = celox::compile_to_sir_with_diagnostic_policy(
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
            &opts
                .diagnostic_policy
                .clone()
                .with_inferred_latches(opts.allow_inferred_latches),
            &Default::default(),
        )
        .map_err(|error| Error::from_reason(error.to_string()))?;
        let laid_out = program
            .into_laid_out_with_mode(opts.four_state, celox::MemoryLayoutMode::ElementStrided);
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

    /// Return a complete initialized memory image for the TypeScript WASM bridge.
    #[napi]
    pub fn initial_memory_bytes(&self) -> Vec<u8> {
        crate::initial_memory::initial_memory_bytes(
            &self.program,
            self.program.layout(),
            self.four_state,
        )
    }

    /// Byte offset where the checkpointable design state starts; it ends at
    /// `stableSize`. The bytes before it hold host data and are never saved.
    #[napi(getter)]
    pub fn state_offset(&self) -> u32 {
        celox::STATE_HEADER_SIZE as u32
    }

    /// Identity of the checkpointable state layout. Memory images can be
    /// exchanged between handles with equal fingerprints.
    #[napi(getter)]
    pub fn state_fingerprint(&self) -> String {
        format!("{:016x}", self.program.state_fingerprint())
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

    /// Evaluate an internal floating-point operation for generated WASM modules.
    #[napi]
    pub fn evaluate_float(&self, lhs: BigInt, rhs: BigInt, code: BigInt) -> BigInt {
        fn bits(value: BigInt) -> u64 {
            let word = value.words.first().copied().unwrap_or(0);
            if value.sign_bit {
                word.wrapping_neg()
            } else {
                word
            }
        }
        napi_bigint(celox::wasm_codegen::evaluate_float(
            bits(lhs),
            bits(rhs),
            bits(code),
        ))
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
    ///
    /// An event without a domain of its own (a clock that only drives a
    /// gated clock) has no module.
    #[napi]
    pub fn event_wasm_bytes(&self, event_name: String) -> Result<Vec<u8>> {
        if let Some(units) = self.event_units(&self.program.sir.eval_apply_ffs, &event_name) {
            let wasm = celox::wasm_codegen::compile_units(
                units,
                self.program.layout(),
                self.four_state,
                false,
            );
            return Ok(wasm.bytes);
        }

        Err(Error::from_reason(format!(
            "Event '{}' has no kernel. Events with kernels: {}",
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

    /// The units of the event named `name` (as `eventsJson` names it, or by
    /// one of its aliases) in one of the program's kernel maps.
    pub(super) fn event_units<'a, U: 'a>(
        &self,
        kernels: impl IntoIterator<Item = (&'a celox::AbsoluteAddr, &'a U)>,
        name: &str,
    ) -> Option<&'a U> {
        let events = &self.program.design.events;
        kernels
            .into_iter()
            .find(|(address, _)| {
                self.program.get_path(address) == name
                    || self.program.get_path(&events.canonical(**address)) == name
            })
            .map(|(_, units)| units)
    }

    /// Per event name, the derived domains its ticks and edges run, as JSON
    /// (`celox::EdgePlans`), for the TypeScript runtime that drives the
    /// event modules itself.
    #[napi]
    pub fn edge_plans_json(&self) -> String {
        serde_json::to_string(&celox::edge_plans(&self.program))
            .unwrap_or_else(|_| "{\"fourState\":false,\"events\":{}}".to_string())
    }

    /// The WASM module of one phase of an event's split kernels: `"evalOnly"`
    /// evaluates its next state without committing, `"apply"` commits it.
    /// Domains that evaluate together use them to sample the same state.
    #[napi]
    pub fn event_phase_wasm_bytes(&self, event_name: String, phase: String) -> Result<Vec<u8>> {
        let units = match phase.as_str() {
            "evalOnly" => &self.program.sir.eval_only_ffs,
            "apply" => &self.program.sir.apply_ffs,
            _ => {
                return Err(Error::from_reason(format!(
                    "unknown event phase '{phase}'; expected 'evalOnly' or 'apply'"
                )));
            }
        };
        self.event_units(units, &event_name)
            .map(|units| {
                celox::wasm_codegen::compile_units(
                    units,
                    self.program.layout(),
                    self.four_state,
                    false,
                )
                .bytes
            })
            .ok_or_else(|| {
                Error::from_reason(format!("event '{event_name}' has no {phase} kernel"))
            })
    }

    /// No-op on wasm32 (no native resources to release).
    #[napi]
    pub fn dispose(&mut self) {}
}

#[cfg(target_arch = "wasm32")]
impl NativeSimulatorHandle {
    pub(super) fn build_four_state_init_regions_json(
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
    pub(super) fn build_layout_json(
        program: &celox::LaidOutProgram,
        layout: &celox::MemoryLayout,
        four_state: bool,
    ) -> String {
        use std::collections::BTreeMap;

        let mut layout_map: BTreeMap<String, serde_json::Value> = BTreeMap::new();

        for addr in program.design.state_objects.keys() {
            let Some(variable) = program.design.variable(addr) else {
                continue;
            };
            let Some(instance) = program.design.instance(addr.instance_id) else {
                continue;
            };
            if !instance.resolves_path_to(&variable.path, *addr) {
                continue;
            }
            let metadata = &program.design.state_objects[addr];
            let Some(&offset) = layout.offsets.get(addr) else {
                continue;
            };
            let name = program.get_path(addr);
            let total_width = layout.widths.get(addr).copied().unwrap_or(0);
            let (width, array_dims) = if metadata.array_dims.is_empty() {
                (total_width, None)
            } else {
                let element_count = metadata.array_dims.iter().product::<usize>();
                (total_width / element_count, Some(&metadata.array_dims))
            };
            let byte_size = celox::get_byte_size(width);
            let mut entry = serde_json::json!({
                "offset": offset,
                "width": width,
                "byte_size": byte_size,
                "is_4state": four_state && metadata.is_4state,
                "direction": layout::direction_str(variable.var_kind),
                "type_kind": layout::type_kind_str(metadata.type_kind),
            });
            if let Some(array_dims) = array_dims {
                entry["array_dims"] = serde_json::json!(array_dims);
            }
            if let Some(array) = layout.unpacked_arrays.get(addr) {
                entry["array_element_stride"] = serde_json::json!(array.element_stride);
                entry["array_plane_size"] = serde_json::json!(array.plane_size);
            }
            layout_map.insert(name, entry);
        }

        serde_json::to_string(&layout_map).unwrap_or_else(|_| "{}".to_string())
    }

    /// Build events JSON from finalized SIR, numbered as the native
    /// simulator numbers them.
    pub(super) fn build_events_json(program: &celox::LaidOutProgram) -> String {
        use std::collections::BTreeMap;

        let mut events: BTreeMap<String, usize> = BTreeMap::new();

        for (next_id, addr) in celox::event_addresses(program).iter().enumerate() {
            let name = program.get_path(addr);
            events.insert(name, next_id);
        }

        serde_json::to_string(&events).unwrap_or_else(|_| "{}".to_string())
    }
}
