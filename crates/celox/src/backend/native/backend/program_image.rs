//! The serializable image of a compiled native program and its runtime schema.

use super::*;

/// One callable function in a packed native code image.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCodeEntry {
    /// Stable name for the emitted function. `clock_alias_update[N]` names
    /// an optional clock-wiring update entry for event N; other names are diagnostic.
    pub name: String,
    /// Byte offset from the start of the native image.
    pub offset: usize,
    /// Size of this function blob, including its private literal data.
    pub size: usize,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(super) struct NativeEventImageRef {
    pub(super) func_offset: usize,
    pub(super) comb_apply_offset: Option<usize>,
    pub(super) addr: AbsoluteAddr,
    pub(super) id: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::backend::native) struct NativeCodeSymbol {
    pub(super) offset: usize,
    pub(super) size: usize,
    pub(super) name: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(super) struct NativeRuntimeOptions {
    pub(super) four_state: bool,
    pub(super) native_tick_loop: bool,
    pub(super) native_force_support: bool,
    pub(super) perf_map: bool,
    /// How lane-partitioned kernels are chosen over sequential ones.
    pub(super) kernel_selection: celox_runtime::parallel::KernelSelection,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::backend::native) struct NativeForceBinding {
    pub signal: celox_runtime::ReflectionSignalId,
    pub enabled: SignalRef,
    pub value: SignalRef,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NativeRuntimeSchema {
    pub(in crate::backend::native) force_bindings: Vec<NativeForceBinding>,
    pub(crate) runtime_errors: HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
    pub(crate) runtime_event_sites: Vec<RuntimeEventSite>,
    /// Extern functions the code calls, resolved when the image is attached.
    pub(crate) extern_functions: Vec<celox_design::ExternFunction>,
    /// Image-local compiled callees appended to the DPI address table: the
    /// function units first, then the generated kernels.
    pub(crate) native_kernel_offsets: Vec<usize>,
    pub(crate) comb_observers: Vec<RuntimeCombObserver<AbsoluteAddr>>,
    pub(crate) testbench_read_roots: HashSet<AbsoluteAddr>,
    pub(crate) rtl_accesses: std::collections::BTreeSet<AbsoluteAddr>,
    pub(crate) persistent_comb_state: std::collections::BTreeSet<AbsoluteAddr>,
    pub(crate) rtl_writes: HashSet<celox_design::VarAtomBase<AbsoluteAddr>>,
    /// Ordered so that the encoded image does not depend on hash order.
    pub(crate) comb_writes: std::collections::BTreeSet<AbsoluteAddr>,
    pub(crate) dead_store_policy: celox_design::DeadStorePolicy,
    /// Ordered so that the encoded image does not depend on hash order.
    pub(crate) live_signals: std::collections::BTreeSet<AbsoluteAddr>,
    pub(crate) declaration_startup: Option<celox_design::DeclarationStartup<AbsoluteAddr>>,
    pub(crate) processes: Vec<celox_design::ProcessSlots<AbsoluteAddr>>,
    pub(crate) process_origins: Vec<celox_design::ProcessOrigin>,
    pub(crate) process_frames: Vec<Option<celox_design::ProcessFrame<AbsoluteAddr>>>,
    pub(crate) process_call_resumes: Vec<Vec<celox_design::CallResume>>,
    pub(crate) function_frames: Vec<celox_design::FunctionFrame>,
    pub(crate) watches: Vec<celox_design::EventWatch<AbsoluteAddr>>,
    pub(crate) occurrence_probes: Vec<(celox_design::VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>,
    pub(crate) wait_sites: Vec<celox_design::WaitSite<AbsoluteAddr>>,
    pub(crate) nba_sites: Vec<celox_design::NbaSite<AbsoluteAddr>>,
    pub(crate) host_request_sites: Vec<celox_design::HostRequestSite<AbsoluteAddr>>,
    pub(crate) active_commits: Vec<AbsoluteAddr>,
    pub(crate) time_slots: Vec<AbsoluteAddr>,
    #[serde(default)]
    pub(crate) sample_shadows: Vec<(AbsoluteAddr, AbsoluteAddr)>,
    #[serde(default)]
    pub(crate) initial_copies: Vec<(AbsoluteAddr, AbsoluteAddr)>,
    pub(crate) scheduler: Option<celox_design::SchedulerSchema<AbsoluteAddr>>,
    pub(crate) derived_edges: Vec<celox_design::DerivedEdgeUnits<AbsoluteAddr>>,
}

/// Pointer-free native compiler artifact which can be attached to the
/// precompiled Celox runtime.
#[derive(Clone, Serialize, Deserialize)]
pub struct NativeProgramImage {
    pub(super) code: NativeCodeBytes,
    pub(super) code_entries: Vec<NativeCodeEntry>,
    pub(super) symbols: Vec<NativeCodeSymbol>,
    pub(super) comb_offset: usize,
    pub(super) comb_unit_offsets: Vec<usize>,
    /// Entry offset of each process kernel.
    pub(super) process_offsets: Vec<usize>,
    pub(super) required_native_features: u8,
    pub(super) event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    pub(super) eval_only_event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    pub(super) apply_event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    pub(super) id_to_addr: Vec<AbsoluteAddr>,
    pub(super) id_to_event: Vec<NativeEventImageRef>,
    pub(super) reflection: DesignReflection,
    pub(super) design: crate::ir::RuntimeDesign,
    pub(super) testbench: Option<TestbenchProgram<AbsoluteAddr>>,
    pub(super) runtime_schema: NativeRuntimeSchema,
    pub(super) layout: MemoryLayout,
    pub(super) native_memory_size: usize,
    pub(super) options: NativeRuntimeOptions,
    pub(super) four_state_inits: Vec<(usize, usize)>,
    /// Lane-partitioned kernels, one function per task.
    pub(super) lanes: Option<super::super::lanes::NativeLaneImage>,
}

// Keep this private Postcard decoder in field order with NativeProgramImage.
// Postcard encodes both Vec<u8> and byte slices as a length followed by raw
// bytes. Borrowing that slice avoids per-byte sequence visits and allows file
// storage to back the code directly. The public Serde representation and payload
// remain unchanged, including for formats that distinguish arrays and bytes.
#[derive(Deserialize)]
#[serde(remote = "NativeProgramImage")]
pub(super) struct PostcardNativeProgramImage {
    #[serde(deserialize_with = "deserialize_postcard_code")]
    pub(super) code: NativeCodeBytes,
    pub(super) code_entries: Vec<NativeCodeEntry>,
    pub(super) symbols: Vec<NativeCodeSymbol>,
    pub(super) comb_offset: usize,
    pub(super) comb_unit_offsets: Vec<usize>,
    /// Entry offset of each process kernel.
    pub(super) process_offsets: Vec<usize>,
    pub(super) required_native_features: u8,
    pub(super) event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    pub(super) eval_only_event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    pub(super) apply_event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    pub(super) id_to_addr: Vec<AbsoluteAddr>,
    pub(super) id_to_event: Vec<NativeEventImageRef>,
    pub(super) reflection: DesignReflection,
    pub(super) design: crate::ir::RuntimeDesign,
    pub(super) testbench: Option<TestbenchProgram<AbsoluteAddr>>,
    pub(super) runtime_schema: NativeRuntimeSchema,
    pub(super) layout: MemoryLayout,
    pub(super) native_memory_size: usize,
    pub(super) options: NativeRuntimeOptions,
    pub(super) four_state_inits: Vec<(usize, usize)>,
    /// Lane-partitioned kernels, one function per task.
    pub(super) lanes: Option<super::super::lanes::NativeLaneImage>,
}

pub(super) fn deserialize_postcard_code<'de, D>(
    deserializer: D,
) -> Result<NativeCodeBytes, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // The caller installs owned or mapped storage after decoding metadata.
    <&[u8]>::deserialize(deserializer).map(|_| NativeCodeBytes::Owned(Vec::new()))
}

impl NativeProgramImage {
    pub(in crate::backend::native) fn from_postcard_bytes(
        bytes: &[u8],
    ) -> Result<Self, postcard::Error> {
        let (code, _) = postcard::take_from_bytes::<&[u8]>(bytes)?;
        let mut image = PostcardNativeProgramImage::deserialize(
            &mut postcard::Deserializer::from_bytes(bytes),
        )?;
        image.code = code.to_vec().into();
        Ok(image)
    }

    #[cfg(target_os = "linux")]
    pub(in crate::backend::native) fn from_mapped_postcard_bytes(
        bytes: &[u8],
        owner: Arc<super::super::executable_mapping::ExecutableMapping>,
    ) -> Result<Self, postcard::Error> {
        let (code, _) = postcard::take_from_bytes::<&[u8]>(bytes)?;
        let mut image = PostcardNativeProgramImage::deserialize(
            &mut postcard::Deserializer::from_bytes(bytes),
        )?;
        if !code.is_empty() {
            // The container reader passes a payload slice from this same owner.
            let start = code
                .as_ptr()
                .addr()
                .checked_sub(owner.as_bytes().as_ptr().addr())
                .ok_or(postcard::Error::DeserializeBadEncoding)?;
            let end = start
                .checked_add(code.len())
                .filter(|&end| end <= owner.as_bytes().len())
                .ok_or(postcard::Error::DeserializeBadEncoding)?;
            image.code = NativeCodeBytes::Mapped {
                owner,
                range: start..end,
            };
        }
        Ok(image)
    }

    /// Physical duration of one tick, retained in serialized native images.
    pub fn time_precision(&self) -> celox_design::TimePrecision {
        self.design.time_precision
    }

    /// Remap filesystem paths in the image's runtime and diagnostic metadata.
    /// This permits relative paths in stored artifacts and rebinding on load.
    /// Machine code and design-authored literal strings are not modified.
    /// A failed mapping can leave some paths remapped; discard that image.
    pub fn try_map_paths<E>(
        &mut self,
        mut map: impl FnMut(&std::path::Path) -> Result<std::path::PathBuf, E>,
    ) -> Result<(), E> {
        if let Some(testbench) = &mut self.testbench {
            testbench.try_map_paths(&mut map)?;
        }
        // The directories memory files are looked up in when they are read.
        let mut initial_state = self.design.take_initial_state();
        let mapped = map_memory_file_bases(&mut initial_state, &mut map);
        self.design.restore_initial_state(initial_state);
        mapped?;
        if let Some(startup) = &mut self.runtime_schema.declaration_startup {
            map_memory_file_bases(&mut startup.ordinary, &mut map)?;
        }
        for base in self.design.memory_file_bases_mut() {
            base.path = map(std::path::Path::new(&base.path))?
                .to_string_lossy()
                .into_owned();
        }
        Ok(())
    }

    /// Complete relocatable machine-code image.
    pub fn code_image(&self) -> &[u8] {
        &self.code
    }

    /// Named entry offsets in [`Self::code_image`].
    pub fn code_entries(&self) -> &[NativeCodeEntry] {
        &self.code_entries
    }

    /// Final state layout consumed by every generated entry.
    pub fn layout(&self) -> &MemoryLayout {
        &self.layout
    }

    /// Source-independent instance hierarchy and signal metadata.
    pub fn reflection(&self) -> &DesignReflection {
        &self.reflection
    }

    /// Design metadata used to name saved state objects.
    pub(crate) fn state_design(&self) -> &crate::ir::RuntimeDesign {
        &self.design
    }

    /// Source-independent runtime diagnostics and combinational observers.
    pub(crate) fn runtime_schema(&self) -> &NativeRuntimeSchema {
        &self.runtime_schema
    }

    /// Whether the image's generated code and state layout use four-state data.
    pub fn four_state(&self) -> bool {
        self.options.four_state
    }

    /// Canonical event-domain topology used by the runtime scheduler.
    pub(crate) fn event_topology(&self) -> &celox_design::EventTopology<AbsoluteAddr> {
        &self.design.events
    }

    /// The storage a reflected signal shares with its aliases: its own, or
    /// for a clock or reset its canonical event domain's.
    pub fn signal_identity(
        &self,
        id: celox_runtime::ReflectionSignalId,
    ) -> Option<super::super::NativeSignalIdentity> {
        let signal = self.reflection.signal(id)?;
        Some(if signal.domain_kind == celox_design::DomainKind::Other {
            super::super::NativeSignalIdentity::State(signal.signal)
        } else {
            super::super::NativeSignalIdentity::Event(
                self.event_topology().canonical(signal.state_address),
            )
        })
    }

    /// Whether the design has process kernels, which only a timed
    /// [`crate::Simulation`] runs.
    pub fn has_processes(&self) -> bool {
        self.runtime_schema
            .processes
            .iter()
            .any(|process| process.kind != celox_design::ProcessKind::Callable)
    }

    /// The compiler-inserted override slots of a reflected signal, as
    /// (enable, value) pairs: setting the enable makes the design read the
    /// value instead of its drivers.
    pub fn force_slots(
        &self,
        id: celox_runtime::ReflectionSignalId,
    ) -> Vec<(SignalRef, SignalRef)> {
        self.runtime_schema
            .force_bindings
            .iter()
            .filter(|binding| binding.signal == id)
            .map(|binding| (binding.enabled, binding.value))
            .collect()
    }

    /// Every reflected signal stored at `identity`, in storage order.
    pub fn signal_aliases(&self, identity: super::super::NativeSignalIdentity) -> Vec<SignalRef> {
        let mut signals = self
            .reflection
            .signals()
            .iter()
            .enumerate()
            .filter_map(|(index, signal)| {
                (self.signal_identity(celox_runtime::ReflectionSignalId(index as u32))
                    == Some(identity))
                .then_some(signal.signal)
            })
            .collect::<Vec<_>>();
        signals.sort_unstable();
        signals.dedup();
        signals
    }

    /// Reconstruct the source-independent runtime metadata retained by this
    /// image. No frontend parsing or SIR/layout work is needed on the
    /// execution side of a host-codegen workflow.
    pub(crate) fn runtime_program(&self) -> crate::ir::RuntimeProgram {
        crate::ir::RuntimeProgram {
            design: self.design.clone(),
            rtl_accesses: self.runtime_schema.rtl_accesses.iter().copied().collect(),
            runtime_schema: RuntimeSchema {
                runtime_errors: self.runtime_schema.runtime_errors.clone(),
                runtime_event_sites: self.runtime_schema.runtime_event_sites.clone(),
                extern_functions: self.runtime_schema.extern_functions.clone(),
                comb_observers: self.runtime_schema.comb_observers.clone(),
                testbench_read_roots: self.runtime_schema.testbench_read_roots.clone(),
                rtl_writes: self.runtime_schema.rtl_writes.clone(),
                comb_writes: self.runtime_schema.comb_writes.iter().copied().collect(),
                persistent_comb_state: self
                    .runtime_schema
                    .persistent_comb_state
                    .iter()
                    .copied()
                    .collect(),
                declaration_startup: self.runtime_schema.declaration_startup.clone(),
                dead_store_policy: self.runtime_schema.dead_store_policy,
                live_signals: self.runtime_schema.live_signals.iter().copied().collect(),
                processes: self.runtime_schema.processes.clone(),
                process_origins: self.runtime_schema.process_origins.clone(),
                process_frames: self.runtime_schema.process_frames.clone(),
                process_call_resumes: self.runtime_schema.process_call_resumes.clone(),
                function_frames: self.runtime_schema.function_frames.clone(),
                watches: self.runtime_schema.watches.clone(),
                occurrence_probes: self.runtime_schema.occurrence_probes.clone(),
                wait_sites: self.runtime_schema.wait_sites.clone(),
                nba_sites: self.runtime_schema.nba_sites.clone(),
                host_request_sites: self.runtime_schema.host_request_sites.clone(),
                active_commits: self.runtime_schema.active_commits.clone(),
                time_slots: self.runtime_schema.time_slots.clone(),
                sample_shadows: self.runtime_schema.sample_shadows.clone(),
                initial_copies: self.runtime_schema.initial_copies.clone(),
                scheduler: self.runtime_schema.scheduler.clone(),
                derived_edges: self.runtime_schema.derived_edges.clone(),
                settle_partitions: None,
            },
            testbench: self.testbench.clone(),
        }
    }

    pub(in crate::backend::native) fn validate(&self) -> Result<(), String> {
        self.design
            .validate()
            .map_err(|error| format!("invalid runtime design: {error}"))?;
        self.reflection
            .validate()
            .map_err(|error| format!("invalid design reflection: {error}"))?;
        for binding in &self.runtime_schema.force_bindings {
            let target = self
                .reflection
                .signal(binding.signal)
                .ok_or("force target is not a reflected signal")?;
            if binding.enabled.width != 1
                || binding.enabled.is_4state
                || binding.value.width != target.signal.width
            {
                return Err("invalid force control shape".into());
            }
            for control in [binding.enabled, binding.value] {
                if control.array_layout.is_some() {
                    return Err("force controls must be packed".into());
                }
                let size = control
                    .width
                    .div_ceil(8)
                    .checked_mul(if control.is_4state { 2 } else { 1 })
                    .ok_or("force control size overflows")?;
                if control
                    .offset
                    .checked_add(size)
                    .is_none_or(|end| end > self.native_memory_size)
                {
                    return Err("force control exceeds native memory".into());
                }
            }
        }
        if self.code.is_empty() {
            return Err("code image is empty".into());
        }
        let mut entry_offsets = HashSet::default();
        let mut previous_end = 0usize;
        for entry in &self.code_entries {
            if !entry.offset.is_multiple_of(NATIVE_CODE_ENTRY_ALIGNMENT) {
                return Err(format!("entry `{}` is not aligned", entry.name));
            }
            if entry.size == 0 {
                return Err(format!("entry `{}` is empty", entry.name));
            }
            let end = entry
                .offset
                .checked_add(entry.size)
                .ok_or_else(|| format!("entry `{}` range overflows", entry.name))?;
            if entry.offset < previous_end || end > self.code.len() {
                return Err(format!("entry `{}` is outside the code image", entry.name));
            }
            if !entry_offsets.insert(entry.offset) {
                return Err(format!("entry `{}` duplicates an offset", entry.name));
            }
            previous_end = end;
        }
        if !entry_offsets.contains(&self.comb_offset) {
            return Err("eval_comb offset does not name an image entry".into());
        }
        for &offset in &self.runtime_schema.native_kernel_offsets {
            if !entry_offsets.contains(&offset) {
                return Err("native kernel offset does not name an image entry".into());
            }
        }
        if self
            .comb_unit_offsets
            .iter()
            .any(|offset| !entry_offsets.contains(offset))
        {
            return Err("a combinational unit offset does not name an image entry".into());
        }
        if self
            .process_offsets
            .iter()
            .any(|offset| !entry_offsets.contains(offset))
        {
            return Err("a process kernel offset does not name an image entry".into());
        }
        // A generated scheduler unit follows the kernels.
        let scheduler_units = self
            .runtime_schema
            .scheduler
            .as_ref()
            .and_then(|scheduler| scheduler.unit)
            .map_or(0, |unit| {
                if unit == self.runtime_schema.processes.len() {
                    1
                } else {
                    usize::MAX
                }
            });
        let testbench_units = self
            .testbench
            .as_ref()
            .and_then(|tb| tb.scheduler())
            .and_then(|scheduler| scheduler.entry)
            .map_or(0, |unit| {
                if unit
                    == self
                        .runtime_schema
                        .processes
                        .len()
                        .saturating_add(scheduler_units)
                {
                    1
                } else {
                    usize::MAX
                }
            });
        // Derived-edge units follow process and scheduler entries.
        let derived_units = 2 * self.runtime_schema.derived_edges.len();
        let derived_in_range = self.runtime_schema.derived_edges.iter().all(|units| {
            units.rising < self.process_offsets.len() && units.falling < self.process_offsets.len()
        });
        if !derived_in_range
            || self.process_offsets.len()
                != self
                    .runtime_schema
                    .processes
                    .len()
                    .saturating_add(scheduler_units)
                    .saturating_add(testbench_units)
                    .saturating_add(derived_units)
        {
            return Err("process kernels and their control slots disagree".into());
        }
        if let Some(tb) = &self.testbench
            && let Some(scheduler) = tb.scheduler()
        {
            let valid_slot = |a: &AbsoluteAddr| {
                self.design
                    .state_objects
                    .get(a)
                    .is_some_and(|info| info.width == 64 && !info.is_4state)
            };
            if scheduler.processes.len() != tb.kernels().len()
                || scheduler.locals.len() != tb.kernels().len()
                || scheduler.clocks.len() != scheduler.events.len()
                || scheduler.clocks.len() != scheduler.periods.len()
                || scheduler.periods.iter().any(|&period| period < 2)
                || !scheduler
                    .control
                    .iter()
                    .chain(scheduler.processes.iter().flatten())
                    .chain(scheduler.clocks.iter().flatten())
                    .all(valid_slot)
                || scheduler
                    .events
                    .iter()
                    .any(|a| !self.design.state_objects.contains_key(a))
                || scheduler
                    .locals
                    .iter()
                    .flatten()
                    .chain(&scheduler.baselines)
                    .any(|(a, b)| {
                        let Some(a) = self.design.state_objects.get(a) else {
                            return true;
                        };
                        let Some(b) = self.design.state_objects.get(b) else {
                            return true;
                        };
                        a.width != b.width
                            || a.array_dims != b.array_dims
                            || a.is_4state != b.is_4state
                    })
            {
                return Err("invalid testbench scheduler storage".into());
            }
        }
        if self.runtime_schema.process_frames.len() != self.runtime_schema.processes.len() {
            return Err("process kernels and their activation frames disagree".into());
        }
        for (slots, frame) in self
            .runtime_schema
            .processes
            .iter()
            .zip(&self.runtime_schema.process_frames)
        {
            let Some(frame) = frame else {
                continue;
            };
            let mut addresses = HashSet::default();
            if ![slots.resume, slots.status, slots.delay, frame.random]
                .iter()
                .all(|slot| frame.private.contains(slot))
                || frame.private.iter().chain(&frame.locals).any(|slot| {
                    !self.design.state_objects.contains_key(slot) || !addresses.insert(*slot)
                })
            {
                return Err("invalid process activation frame storage".into());
            }
            let mut entries = HashSet::default();
            for probe in &frame.event_probes {
                if probe.resume == 0
                    || probe.resume as usize >= frame.resume_count
                    || !entries.insert(probe.resume)
                    || !frame.private.contains(&probe.result)
                    || !probe.scratch.contains(&probe.result)
                    || probe
                        .scratch
                        .iter()
                        .any(|slot| !frame.private.contains(slot))
                    || probe.inputs.iter().any(|slot| !frame.locals.contains(slot))
                    || probe.inputs.iter().collect::<HashSet<_>>().len() != probe.inputs.len()
                {
                    return Err("invalid process event-expression probe".into());
                }
            }
        }
        if self.required_native_features & !KNOWN_NATIVE_FEATURES != 0 {
            return Err("native image contains unknown feature requirements".into());
        }
        if let Some(lanes) = &self.lanes {
            lanes.validate(&entry_offsets, &self.event_map)?;
        }
        for symbol in &self.symbols {
            let end = symbol
                .offset
                .checked_add(symbol.size)
                .ok_or_else(|| format!("symbol `{}` range overflows", symbol.name))?;
            if symbol.size == 0 || end > self.code.len() {
                return Err(format!(
                    "symbol `{}` is outside the code image",
                    symbol.name
                ));
            }
        }
        for event in self
            .event_map
            .values()
            .chain(self.eval_only_event_map.values())
            .chain(self.apply_event_map.values())
            .chain(self.id_to_event.iter())
        {
            if !entry_offsets.contains(&event.func_offset)
                || event
                    .comb_apply_offset
                    .is_some_and(|offset| !entry_offsets.contains(&offset))
            {
                return Err(format!(
                    "event {} references a missing image entry",
                    event.id
                ));
            }
        }
        let semantic_size = self
            .layout
            .merged_total_size
            .checked_add(self.layout.triggered_bits_total_size)
            .ok_or_else(|| "semantic memory size overflows".to_string())?;
        if let Some(trace) = &self.layout.trace {
            let metadata_start = self
                .layout
                .triggered_bits_offset
                .checked_add(self.layout.triggered_bits_total_size)
                .ok_or_else(|| "trace metadata offset overflows".to_string())?;
            if !trace.validate(
                self.layout.total_size,
                metadata_start,
                self.layout.scratch_base_offset,
            ) {
                return Err("invalid waveform activity layout".into());
            }
        }
        if self.native_memory_size < semantic_size {
            return Err("native memory is smaller than the semantic state".into());
        }
        for &(offset, size) in &self.four_state_inits {
            let end = size
                .checked_mul(2)
                .and_then(|size| offset.checked_add(size))
                .ok_or_else(|| "four-state initialization range overflows".to_string())?;
            if end > self.native_memory_size {
                return Err("four-state initialization exceeds native memory".into());
            }
        }
        Ok(())
    }
}

// ────────────────────────────────────────────────────────────────
// Compilation
// ────────────────────────────────────────────────────────────────

pub(super) fn build_force_bindings(
    program: &crate::ir::RuntimeProgram,
    layout: &crate::backend::MemoryLayout,
    paths: &[String],
) -> Result<Vec<NativeForceBinding>, SimulatorError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let reflection = program.build_design_reflection(layout);
    let signal_ref = |addr: &AbsoluteAddr| SignalRef {
        offset: layout.offsets[addr],
        width: layout.widths[addr],
        is_4state: layout.is_4states[addr],
        array_layout: None,
    };
    let mut result = Vec::new();
    for path in paths {
        let (index, target) = reflection
            .signals()
            .iter()
            .enumerate()
            .find(|(_, signal)| signal.full_name == *path)
            .ok_or_else(|| {
                codegen_err(CodegenError::message(format!(
                    "unknown force signal `{path}`"
                )))
            })?;
        if !target.unpacked_dims.is_empty() || target.domain_kind != celox_design::DomainKind::Other
        {
            return Err(codegen_err(CodegenError::message(format!(
                "force signal `{path}` must be a packed non-clock variable"
            ))));
        }
        let mut found = false;
        for instance in program.design.instances() {
            for address in instance.state_addresses() {
                let var = program.design.variable(address).unwrap();
                if instance.resolves_path_to(&var.path, *address)
                    && var.path.len() > 1
                    && var.var_kind.is_port()
                    && layout.offsets[address] == target.signal.offset
                {
                    return Err(codegen_message(format!(
                        "force signal `{path}` is a structured port alias; select the owning interface variable"
                    )));
                }
                if layout.offsets[address] != target.signal.offset
                    || layout.widths[address] != target.signal.width
                {
                    continue;
                }
                let flat_name = var.path.join("$");
                let control = |prefix: &str| {
                    instance.state_addresses().iter().find(|addr| {
                        program.design.variable(addr).unwrap().path
                            == [format!("{prefix}{flat_name}")]
                    })
                };
                if let (Some(enabled), Some(value)) =
                    (control("@force-enable:"), control("@force-value:"))
                {
                    result.push(NativeForceBinding {
                        signal: celox_runtime::ReflectionSignalId(index as u32),
                        enabled: signal_ref(enabled),
                        value: signal_ref(value),
                    });
                    found = true;
                }
            }
        }
        if !found {
            return Err(codegen_err(CodegenError::message(format!(
                "no force instrumentation for `{path}`"
            ))));
        }
    }
    Ok(result)
}

/// Remap the base directories of the memory files `values` load and the
/// source files of their calls.
fn map_memory_file_bases<E>(
    values: &mut [celox_design::InitialStateValue<AbsoluteAddr>],
    map: &mut impl FnMut(&std::path::Path) -> Result<std::path::PathBuf, E>,
) -> Result<(), E> {
    for value in values {
        let celox_design::InitialStateData::MemoryFile(load) = &mut value.data else {
            continue;
        };
        let location = load.location.as_mut().map(|location| &mut location.file);
        for path in load.base.iter_mut().chain(location) {
            if !path.is_empty() {
                *path = map(std::path::Path::new(path))?
                    .to_string_lossy()
                    .into_owned();
            }
        }
    }
    Ok(())
}
