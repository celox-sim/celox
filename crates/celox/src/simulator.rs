mod builder;
#[cfg(feature = "host-runtime")]
mod checkpoint;
mod error;
#[cfg(feature = "host-runtime")]
mod heap;
#[cfg(feature = "systemverilog")]
pub(crate) mod mixed;
#[cfg(feature = "host-runtime")]
mod state_file;

#[cfg(all(
    feature = "host-runtime",
    any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )
))]
pub use builder::NativeCompilation;
#[cfg(feature = "host-runtime")]
pub use builder::{DeadStorePolicy, SimulatorBuilder, SimulatorOptions, TierPromotion};
pub use builder::{
    compile_frontend_to_sir, compile_to_sir, compile_to_sir_with_diagnostic_policy,
    compile_to_sir_with_file_dependencies, compile_to_sir_with_latch_policy,
};
#[cfg(feature = "systemverilog")]
pub use builder::{
    compile_mixed_to_sir, compile_mixed_to_sir_with_file_dependencies, compile_sv_to_sir,
};
#[cfg(feature = "host-runtime")]
pub use checkpoint::{Checkpoint, CheckpointError, StateImage};
pub use error::render_diagnostic;
pub use error::{CodegenError, CompilationWarning, SimulatorError, SimulatorErrorKind};
#[cfg(feature = "host-runtime")]
pub use state_file::{StateError, StateMismatch, StateSchema};

#[cfg(feature = "host-runtime")]
mod host {
    mod declaration;
    mod distribution;
    mod file_io;
    mod fread;
    mod scan;
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use crate::backend::RuntimeEventBuffer;
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    use crate::backend::native::{NativeBackend, SharedNativeCode};
    use crate::{
        IOContext, RuntimeErrorCode,
        backend::{JitBackend, MemoryLayout, SharedJitCode, SimBackend},
        ir::{
            DisplaySizing, InitialMemoryData, InitialMemoryWriteRun, InstancePath,
            RuntimeEventKind, RuntimeEventSite, RuntimeProgram, SignalRef, VariableInfo,
        },
    };
    use celox_design::{AssertionSite, ReportLocation, Severity};
    use celox_runtime::backend::EventHandle as _;
    use celox_testbench::{
        DisplayFormatArg, FieldSpec, format_sv_display_arg, format_veryl_display_arg,
        pad_veryl_field,
    };
    use num_bigint::BigUint;

    #[derive(Default)]
    pub(crate) struct SvVcdControl {
        pub path: Option<std::path::PathBuf>,
        pub started: Option<u64>,
        pub pending: bool,
        pub selections: Vec<crate::VcdSignalDesc>,
        pub external_selections: Vec<(usize, celox_runtime::VcdExternalSignalDesc)>,
        pub source_writer: bool,
        pub active_external_indices: Vec<usize>,
        pub limit: u64,
        pub suspended: bool,
    }

    /// Hierarchical instance tree with resolved signals.
    #[derive(Debug, Clone)]
    pub struct InstanceHierarchy {
        pub module_name: String,
        /// The index of this instance under its name: its element index in
        /// an instance array or generate loop, 0 otherwise.
        pub index: usize,
        /// Whether this instance's name takes an index (an instance array or
        /// generate loop), even when it is the only instance under the name.
        pub indexed: bool,
        pub signals: Vec<NamedSignal>,
        pub children: Vec<(String, Vec<InstanceHierarchy>)>,
    }

    /// A named signal with its resolved memory reference and metadata.
    #[derive(Debug, Clone)]
    pub struct NamedSignal {
        pub name: String,
        pub signal: SignalRef,
        pub info: VariableInfo,
        /// For reset signals, the name of the associated clock (from FfDeclaration).
        pub associated_clock: Option<String>,
    }

    /// A named event with its resolved ID and event reference.
    #[derive(Debug, Clone)]
    pub struct NamedEvent<B: SimBackend = crate::DefaultBackend> {
        pub name: String,
        pub id: usize,
        pub event_ref: B::Event,
    }

    /// The core logic evaluation engine.
    ///
    /// Encapsulates the backend, the original SIR program,
    /// and an optional VCD writer. Provides low-level, event-driven control.
    ///
    /// The default type parameter `B = DefaultBackend` means that bare `Simulator`
    /// uses the custom native backend on a matching host and Cranelift for
    /// cross-codegen builds or unsupported hosts.
    pub struct Simulator<B: SimBackend = crate::DefaultBackend> {
        pub(crate) backend: B,
        /// Byte offsets of every handle slot in state memory.
        pub(super) handle_slots: std::sync::OnceLock<Arc<[(usize, celox_heap::HandleKind)]>>,
        /// Set while handles live outside state memory, where no collection
        /// may run: between the polls of concurrent testbench processes.
        pub(crate) heap_safepoints_blocked: bool,
        file_io: celox_runtime::file_io::FileIo,
        pending_host_events: Arc<std::sync::Mutex<PendingRuntimeEvents>>,
        pub(crate) program: RuntimeProgram,
        /// The program's scheduling facts.
        pub(crate) schedule: celox_design::schedule::ScheduleModel<crate::AbsoluteAddr>,
        pub(crate) vcd_writer: Option<crate::VcdWriter>,
        pub(crate) sv_vcd: SvVcdControl,
        pub(crate) dirty: bool,
        /// Static declaration kernels may stop before wildcard baselines are
        /// armed. Host reads and event drains must not settle comb meanwhile.
        pub(crate) declaration_phase: bool,
        pub(crate) warnings: Vec<CompilationWarning>,
        pub(crate) components: crate::component::ComponentRuntime,
        pub(crate) component_simulation: Option<celox_runtime::SimulationState<B>>,
        runtime_event_read_seq: Arc<AtomicU64>,
        runtime_event_drain_active: Arc<AtomicBool>,
        pub(super) comb_observer_snapshots: Vec<crate::comb_activation::ObserverSnapshot>,
        /// Storage reused for the next snapshot.
        comb_observer_scratch: Vec<crate::comb_activation::ObserverSnapshot>,
        comb_observer_reads: crate::comb_activation::ObserverReads,
        pub(super) comb_observer_initial_eval: bool,
        /// The design has deferred assertions, whose actions mature at the
        /// end of each time slot.
        pub(crate) deferred_assertions: bool,
        /// The activation groups of combinational processes with deferred
        /// assertions, with a site of each: running the process again
        /// flushes its queue (IEEE 1800-2023 16.4.2).
        comb_deferred_groups: Vec<(u32, usize)>,
        /// A timed simulation owns this simulator: a time slot ends when its
        /// scheduler says so, not when combinational logic settles.
        pub(crate) timed: bool,
        /// Sampled variables and their copies
        /// (`RuntimeSchema::sample_shadows`).
        sample_shadows: Vec<(SignalRef, SignalRef)>,
        /// Deferred assertions or sampled copies have work when a host
        /// call's time slot is over.
        host_slot_work: bool,
        /// Identity of the state layout, computed on first checkpoint use.
        pub(super) checkpoint_fingerprint: std::sync::OnceLock<u64>,
        /// State objects as state files name them, built on first use.
        pub(super) state_schema: std::sync::OnceLock<Arc<state_file::StateSchema>>,
        pub(crate) diagnostics: crate::RuntimeDiagnostics,
        pub(crate) plusargs: celox_runtime::plusargs::Plusargs,
        tick_timing_ticks: u64,
        tick_timing_eval_apply_ns: u64,
        tick_timing_eval_comb_ns: u64,
        /// Per event ID, the derived clock domains a tick can trigger.
        /// Empty for a design without derived clocks.
        derived_ticks: celox_runtime::DerivedTickPlans<B::Event>,
        /// Boxed, so that taking it out for a tick moves one pointer.
        derived_scratch: Option<Box<celox_runtime::DerivedTickScratch>>,
        /// Per event ID, the generated units that run an edge of the event
        /// with its derived domains. Empty without them.
        derived_units: Arc<[Option<DerivedEdgeUnit>]>,
        /// The flag a derived-edge unit reads to settle combinational logic
        /// before the domains evaluate.
        derived_settle: Option<SignalRef>,
        /// The design finished a timed simulation (`$finish`). Evaluation,
        /// host writes and waveform dumps no longer change anything.
        pub(crate) frozen: bool,
        /// Host writes a timed simulation observes one at a time.
        pub(crate) host_watch: celox_runtime::HostWatch,
        /// Runtime-event sequence up to which a timed simulation looked for
        /// `$finish` records.
        pub(crate) finish_scan_seq: u64,
        /// Whether a timed simulation runs the design and it has a `$finish`
        /// runtime event site.
        pub(crate) finish_sites: bool,
        /// Per event ID, its signal and polarity for explicit edges.
        event_edges: celox_runtime::EventEdges,
    }

    /// Why [`Simulator::try_dump`] failed.
    #[derive(Debug, thiserror::Error)]
    pub enum DumpError {
        #[error("evaluating combinational logic failed: {0}")]
        Runtime(RuntimeErrorCode),
        #[error(transparent)]
        Io(std::io::Error),
    }

    /// HDL output is an arbitrary sequence of 8-bit character codes.
    /// Message payloads are byte-exact; decode explicitly if text is required.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RuntimeEvent {
        Display {
            message: Vec<u8>,
        },
        Write {
            message: Vec<u8>,
        },
        /// A message of a severity task or of a Veryl assertion. A
        /// [`Severity::Fatal`] report also ends the simulation with an error.
        Report(Report),
        /// The design executed `$finish`. The host decides whether to stop.
        Finish,
        /// The design executed `$stop` outside a process kernel. The host
        /// decides whether to suspend.
        Stop,
        Missed {
            count: u64,
        },
    }

    /// What a severity task reports (IEEE 1800-2023 20.10).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Report {
        pub severity: Severity,
        /// The formatted message, byte-exact like [`RuntimeEvent::Display`].
        pub message: Vec<u8>,
        /// The simulation time of the report, when the drain knows it.
        pub time: Option<u64>,
        /// The hierarchical name of the scope that issued the report.
        pub scope: Option<String>,
        /// The file and line of the task call.
        pub location: Option<ReportLocation>,
        /// The assertion whose action block issued the report.
        pub assertion: Option<AssertionSite>,
    }

    impl Report {
        /// A report with no time, scope, location or assertion.
        pub fn new(severity: Severity, message: Vec<u8>) -> Self {
            Self {
                severity,
                message,
                time: None,
                scope: None,
                location: None,
                assertion: None,
            }
        }

        /// The report as a tool prints it: the severity, the file and line,
        /// the message, then the time and the scope (IEEE 1800-2023 20.10).
        pub fn render(&self) -> Vec<u8> {
            let mut out = format!("{:?}", self.severity).into_bytes();
            if let Some(location) = &self.location {
                out.extend_from_slice(format!(": {}:{}", location.file, location.line).as_bytes());
            }
            out.extend_from_slice(b": ");
            out.extend_from_slice(&self.message);
            let mut context = Vec::new();
            if let Some(time) = self.time {
                context.push(format!("Time: {time}"));
            }
            let name = match (
                &self.scope,
                self.assertion.as_ref().and_then(|a| a.label.as_ref()),
            ) {
                (Some(scope), Some(label)) => Some(format!("{scope}.{label}")),
                (scope, label) => scope.clone().or_else(|| label.cloned()),
            };
            if let Some(name) = name {
                context.push(format!("Scope: {name}"));
            }
            if !context.is_empty() {
                out.extend_from_slice(format!("\n  {}", context.join("  ")).as_bytes());
            }
            out
        }
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RuntimeFormatContext<'a> {
        pub tb_time: Option<u64>,
        pub scope: Option<&'a str>,
    }

    pub struct RuntimeEventDrain {
        buffer: Arc<RuntimeEventBuffer>,
        layout: MemoryLayout,
        sites: Vec<RuntimeEventSite>,
        pending_host_events: Arc<std::sync::Mutex<PendingRuntimeEvents>>,
        read_seq: u64,
        shared_read_seq: Arc<AtomicU64>,
        active: Arc<AtomicBool>,
    }

    impl RuntimeEventDrain {
        pub fn drain(&mut self) -> Vec<RuntimeEvent> {
            self.drain_with_context(RuntimeFormatContext::default())
        }

        pub fn drain_with_context(&mut self, ctx: RuntimeFormatContext<'_>) -> Vec<RuntimeEvent> {
            let mut pending = self.pending_host_events.lock().unwrap();
            self.read_seq = self.shared_read_seq.load(Ordering::Acquire);
            let events = drain_raw_runtime_events_from_buffer(
                &self.buffer,
                &self.layout,
                &self.sites,
                &mut self.read_seq,
            );
            self.shared_read_seq.store(self.read_seq, Ordering::Release);
            pending.extend(events, self.layout.runtime_event_capacity);
            let events = pending.drain();
            drop(pending);
            events
                .into_iter()
                .filter_map(|raw| render_raw_runtime_event(raw, &self.sites, ctx))
                .collect()
        }
    }

    impl Drop for RuntimeEventDrain {
        fn drop(&mut self) {
            self.active.store(false, Ordering::Release);
        }
    }

    impl<B: SimBackend> std::fmt::Debug for Simulator<B> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Simulator").finish()
        }
    }

    #[derive(Clone)]
    struct RuntimeEventArgValue {
        values: Vec<u64>,
        masks: Vec<u64>,
        width: usize,
        signed: bool,
        is_string: bool,
        float_precision: Option<celox_design::FloatPrecision>,
        /// A string handle not yet replaced by its contents.
        handle: bool,
    }

    impl RuntimeEventArgValue {
        /// Replace a string handle by the bytes of its string, which the
        /// formatter prints as text.
        fn resolve_handle(&mut self, heap: &celox_heap::Heap) {
            if !std::mem::take(&mut self.handle) {
                return;
            }
            let handle = self.values.first().copied().unwrap_or(0) as celox_heap::Handle;
            let bytes = heap.string(handle);
            self.values = BigUint::from_bytes_be(bytes).to_u64_digits();
            self.masks = vec![0; self.values.len()];
            self.width = bytes.len() * 8;
            self.signed = false;
            self.is_string = true;
        }
    }

    impl RawRuntimeEvent {
        fn resolve_handles(&mut self, heap: &celox_heap::Heap) {
            let (Self::Host { args, .. } | Self::Event { args, .. }) = self else {
                return;
            };
            for arg in args {
                arg.resolve_handle(heap);
            }
        }
    }

    #[derive(Clone)]
    enum RawRuntimeEvent {
        Host {
            site: Box<RuntimeEventSite>,
            args: Vec<RuntimeEventArgValue>,
        },
        Event {
            site_id: usize,
            args: Vec<RuntimeEventArgValue>,
        },
        Missed {
            count: u64,
        },
    }

    /// A bounded continuation of the backend ring. Records remain unformatted
    /// until the eventual consumer supplies its scope/time context.
    #[derive(Clone, Default)]
    struct PendingRuntimeEvents {
        events: std::collections::VecDeque<RawRuntimeEvent>,
        missed: u64,
        deferred: DeferredQueues,
    }

    /// The actions of deferred assertions that wait in the queue of their
    /// process until they mature (IEEE 1800-2023 16.4.1).
    #[derive(Clone, Default)]
    struct DeferredQueues {
        /// How the events of each site are deferred, and whether the site is
        /// fatal, by site id. Empty without deferred assertions.
        sites: Vec<Option<(celox_design::DeferredSite, bool)>>,
        /// Waiting actions in issue order, with their queue and region.
        waiting: Vec<(u32, celox_design::AssertionDeferral, RawRuntimeEvent)>,
        /// Events lost to the ring since the last maturity, in a design with
        /// fatal deferred actions: they may have held one.
        missed: u64,
    }

    impl PendingRuntimeEvents {
        fn extend(&mut self, events: impl IntoIterator<Item = RawRuntimeEvent>, capacity: usize) {
            for event in events {
                self.push(event, capacity);
            }
        }

        /// Queue the event, unless it is the action of a deferred assertion,
        /// which waits, or a flush point, which discards the actions waiting
        /// in its queue (16.4.2).
        fn push(&mut self, event: RawRuntimeEvent, capacity: usize) {
            if let RawRuntimeEvent::Event { site_id, .. } = &event
                && let Some(Some((deferred, _))) = self.deferred.sites.get(*site_id)
            {
                let site = *site_id as u32;
                match *deferred {
                    celox_design::DeferredSite::FlushPoint => self.flush(site),
                    celox_design::DeferredSite::Action { queue, region } => {
                        self.deferred.waiting.push((queue, region, event));
                    }
                }
                return;
            }
            if let RawRuntimeEvent::Missed { count } = event
                && self
                    .deferred
                    .sites
                    .iter()
                    .flatten()
                    .any(|&(_, fatal)| fatal)
            {
                self.deferred.missed += count;
            }
            self.push_ready(event, capacity);
        }

        /// Discard the actions waiting in queue `queue`.
        fn flush(&mut self, queue: u32) {
            self.deferred
                .waiting
                .retain(|&(waiting, ..)| waiting != queue);
        }

        /// The time slot ends: queue the waiting actions, those of the
        /// Observed region (`#0`) before those of the Postponed region
        /// (`final`). Returns the first fatal one, or [`RawRuntimeEvent::Missed`]
        /// when events that may have been one were lost.
        fn mature(&mut self, capacity: usize) -> Option<RawRuntimeEvent> {
            let waiting = std::mem::take(&mut self.deferred.waiting);
            let missed = std::mem::take(&mut self.deferred.missed);
            let mut fatal = (missed > 0).then_some(RawRuntimeEvent::Missed { count: missed });
            for region in [
                celox_design::AssertionDeferral::Observed,
                celox_design::AssertionDeferral::Final,
            ] {
                for (_, _, event) in waiting.iter().filter(|(_, r, _)| *r == region) {
                    if fatal.is_none()
                        && let RawRuntimeEvent::Event { site_id, .. } = event
                        && let Some(Some((_, true))) = self.deferred.sites.get(*site_id)
                    {
                        fatal = Some(event.clone());
                    }
                    self.push_ready(event.clone(), capacity);
                }
            }
            fatal
        }

        fn push_ready(&mut self, event: RawRuntimeEvent, capacity: usize) {
            if let RawRuntimeEvent::Missed { count } = event {
                // Ring overwrite means every older pending record also lies
                // outside the latest `capacity` records of the combined stream.
                self.missed += count + self.events.len() as u64;
                self.events.clear();
            } else if capacity == 0 {
                self.missed += 1;
            } else {
                if self.events.len() == capacity {
                    self.events.pop_front();
                    self.missed += 1;
                }
                self.events.push_back(event);
            }
        }

        fn drain(&mut self) -> Vec<RawRuntimeEvent> {
            let mut events = Vec::with_capacity(self.events.len() + usize::from(self.missed > 0));
            if self.missed > 0 {
                events.push(RawRuntimeEvent::Missed {
                    count: std::mem::take(&mut self.missed),
                });
            }
            events.extend(self.events.drain(..));
            events
        }
    }

    fn runtime_event_words_to_biguint(words: &[u64], width: usize) -> BigUint {
        let mut value = BigUint::from(0u8);
        for (idx, word) in words.iter().enumerate() {
            value |= BigUint::from(*word) << (idx * 64);
        }
        if width > 0 {
            value & ((BigUint::from(1u8) << width) - BigUint::from(1u8))
        } else {
            BigUint::from(0u8)
        }
    }

    fn write_bits_to_memory(mem: &mut [u8], dst_bit_offset: usize, bit_width: usize, src: &[u8]) {
        write_bits_to_memory_from(mem, dst_bit_offset, bit_width, src, 0);
    }

    fn write_bits_to_memory_from(
        mem: &mut [u8],
        dst_bit_offset: usize,
        bit_width: usize,
        src: &[u8],
        src_bit_offset: usize,
    ) {
        for bit in 0..bit_width {
            let src_bit_index = src_bit_offset + bit;
            let src_bit = (src[src_bit_index / 8] >> (src_bit_index % 8)) & 1;
            let dst_idx = (dst_bit_offset + bit) / 8;
            let dst_mask = 1u8 << ((dst_bit_offset + bit) % 8);
            if src_bit == 0 {
                mem[dst_idx] &= !dst_mask;
            } else {
                mem[dst_idx] |= dst_mask;
            }
        }
    }

    fn write_initial_run_to_plane(
        mem: &mut [u8],
        signal: SignalRef,
        mask_plane: bool,
        run: &InitialMemoryWriteRun,
        src: &[u8],
    ) {
        let Some(array) = signal.array_layout else {
            let plane_size = signal.width.div_ceil(8);
            let plane_bit_offset =
                (signal.offset + usize::from(mask_plane) * plane_size) * 8 + run.bit_offset;
            write_bits_to_memory(mem, plane_bit_offset, run.bit_width, src);
            return;
        };

        let plane_offset = signal.offset + usize::from(mask_plane) * array.plane_size;
        let mut consumed = 0usize;
        while consumed < run.bit_width {
            let logical_offset = run.bit_offset + consumed;
            let element = logical_offset / array.element_width;
            let intra_element = logical_offset % array.element_width;
            let part_width = (run.bit_width - consumed).min(array.element_width - intra_element);
            let destination_bit_offset =
                (plane_offset + element * array.element_stride) * 8 + intra_element;

            if consumed.is_multiple_of(8)
                && destination_bit_offset.is_multiple_of(8)
                && part_width.is_multiple_of(8)
            {
                let src_byte = consumed / 8;
                let dst_byte = destination_bit_offset / 8;
                let byte_width = part_width / 8;
                mem[dst_byte..dst_byte + byte_width]
                    .copy_from_slice(&src[src_byte..src_byte + byte_width]);
            } else {
                write_bits_to_memory_from(mem, destination_bit_offset, part_width, src, consumed);
            }
            consumed += part_width;
        }
    }

    fn runtime_event_format_arg(
        arg: &RuntimeEventArgValue,
        spec: char,
        sizing: DisplaySizing,
        field: FieldSpec,
    ) -> Vec<u8> {
        let value = runtime_event_words_to_biguint(&arg.values, arg.width);
        let mask = runtime_event_words_to_biguint(&arg.masks, arg.width);
        let arg = DisplayFormatArg {
            value: &value,
            mask: Some(&mask),
            width: arg.width,
            signed: arg.signed,
            is_string: arg.is_string,
            float_precision: arg.float_precision,
        };
        match sizing {
            DisplaySizing::Veryl => format_veryl_display_arg(&arg, spec, field),
            DisplaySizing::Ieee => format_sv_display_arg(&arg, spec, field),
        }
    }

    fn render_runtime_event_message(
        site: &RuntimeEventSite,
        args: &[RuntimeEventArgValue],
        ctx: RuntimeFormatContext<'_>,
    ) -> Vec<u8> {
        let Some(template) = site.template.as_deref() else {
            let default_spec = match site.kind {
                // Veryl prints arguments without a format string in hexadecimal.
                RuntimeEventKind::Display
                | RuntimeEventKind::Write
                | RuntimeEventKind::Finish
                | RuntimeEventKind::Stop
                    if site.sizing == DisplaySizing::Veryl =>
                {
                    'x'
                }
                RuntimeEventKind::Display
                | RuntimeEventKind::Write
                | RuntimeEventKind::Finish
                | RuntimeEventKind::Stop => 'd',
                RuntimeEventKind::Report(_) => {
                    if args.is_empty() {
                        return b"assertion failed".to_vec();
                    }
                    'x'
                }
            };
            return args
                .iter()
                .map(|arg| {
                    runtime_event_format_arg(
                        arg,
                        if arg.float_precision.is_some() {
                            'g'
                        } else {
                            default_spec
                        },
                        site.sizing,
                        FieldSpec::default(),
                    )
                })
                .collect::<Vec<_>>()
                .join(&b' ');
        };
        let mut out = Vec::new();
        let mut arg_idx = 0usize;
        let mut chars = template.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '%' {
                out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                continue;
            }
            if matches!(chars.peek(), Some('%')) {
                chars.next();
                out.push(b'%');
                continue;
            }
            let field = FieldSpec::parse(&mut chars, true);
            let spec = chars.next().unwrap_or('d');
            match spec {
                'x' | 'h' | 'X' | 'H' | 'b' | 'B' | 'o' | 'O' | 'c' | 'C' | 's' | 'S' => {
                    let Some(arg) = args.get(arg_idx) else {
                        arg_idx += 1;
                        continue;
                    };
                    out.extend_from_slice(&runtime_event_format_arg(arg, spec, site.sizing, field));
                    arg_idx += 1;
                }
                'd' | 'D' | 'i' | 'I' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                    let Some(arg) = args.get(arg_idx) else {
                        arg_idx += 1;
                        continue;
                    };
                    out.extend_from_slice(&runtime_event_format_arg(arg, spec, site.sizing, field));
                    arg_idx += 1;
                }
                't' | 'T' if site.sizing == DisplaySizing::Ieee => {
                    let Some(arg) = args.get(arg_idx) else {
                        arg_idx += 1;
                        continue;
                    };
                    let field = FieldSpec {
                        width: Some(field.width.unwrap_or(20)),
                        ..field
                    };
                    out.extend_from_slice(&runtime_event_format_arg(arg, 'd', site.sizing, field));
                    arg_idx += 1;
                }
                't' | 'T' | 'm' | 'M' => {
                    let text = if matches!(spec, 't' | 'T') {
                        ctx.tb_time.unwrap_or(0).to_string()
                    } else {
                        ctx.scope
                            .or(site.scope.as_deref())
                            .unwrap_or("<hierarchy>")
                            .to_string()
                    };
                    out.extend_from_slice(&match site.sizing {
                        DisplaySizing::Veryl => pad_veryl_field(text.into_bytes(), spec, field),
                        DisplaySizing::Ieee => text.into_bytes(),
                    });
                }
                other => {
                    out.push(b'%');
                    out.extend_from_slice(other.encode_utf8(&mut [0; 4]).as_bytes());
                }
            }
        }
        out
    }

    fn render_raw_runtime_event(
        raw: RawRuntimeEvent,
        sites: &[RuntimeEventSite],
        ctx: RuntimeFormatContext<'_>,
    ) -> Option<RuntimeEvent> {
        match raw {
            RawRuntimeEvent::Missed { count } => Some(RuntimeEvent::Missed { count }),
            RawRuntimeEvent::Event { site_id, args } => {
                Some(render_runtime_event(sites.get(site_id)?, &args, ctx))
            }
            RawRuntimeEvent::Host { site, args } => Some(render_runtime_event(&site, &args, ctx)),
        }
    }

    fn render_runtime_event(
        site: &RuntimeEventSite,
        args: &[RuntimeEventArgValue],
        ctx: RuntimeFormatContext<'_>,
    ) -> RuntimeEvent {
        let message = render_runtime_event_message(site, args, ctx);
        match site.kind {
            RuntimeEventKind::Display => RuntimeEvent::Display { message },
            RuntimeEventKind::Write => RuntimeEvent::Write { message },
            RuntimeEventKind::Report(severity) => RuntimeEvent::Report(Report {
                severity,
                message,
                time: ctx.tb_time,
                scope: ctx.scope.map(str::to_owned).or_else(|| site.scope.clone()),
                location: site.location.clone(),
                assertion: site.assertion.clone(),
            }),
            RuntimeEventKind::Finish => RuntimeEvent::Finish,
            RuntimeEventKind::Stop => RuntimeEvent::Stop,
        }
    }

    fn collect_runtime_events(
        layout: &MemoryLayout,
        sites: &[RuntimeEventSite],
        read_seq: &mut u64,
        buffer_size: usize,
        mut read_payload_u64: impl FnMut(usize) -> u64,
        mut read_seq_u64: impl FnMut(usize) -> u64,
    ) -> Vec<RawRuntimeEvent> {
        use crate::backend::memory_layout::{
            RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
            RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET, RUNTIME_EVENT_SLOT_SEQ_OFFSET,
            RUNTIME_EVENT_SLOT_SITE_OFFSET, RUNTIME_EVENT_WRITING,
        };

        if RUNTIME_EVENT_HEADER_SIZE > buffer_size || layout.runtime_event_capacity == 0 {
            return Vec::new();
        }

        // Ring-buffer synchronization protocol:
        // - writers store event payload/site/arg fields normally;
        // - writers publish slot/global sequence words with release semantics;
        // - readers acquire-load sequence words, then read payload normally;
        // - readers re-check the slot sequence after reading payload to reject races.
        let write_seq = read_seq_u64(0);
        let mut events = Vec::new();
        let capacity = layout.runtime_event_capacity as u64;
        if *read_seq + capacity < write_seq {
            let new_read = write_seq - capacity;
            events.push(RawRuntimeEvent::Missed {
                count: new_read - *read_seq,
            });
            *read_seq = new_read;
        }

        while *read_seq < write_seq {
            let seq = *read_seq;
            let slot = (seq as usize) & (layout.runtime_event_capacity - 1);
            let slot_base = RUNTIME_EVENT_HEADER_SIZE + slot * layout.runtime_event_slot_size;
            let published = read_seq_u64(slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET);
            if published == RUNTIME_EVENT_WRITING || published != seq {
                break;
            }

            let site_id = read_payload_u64(slot_base + RUNTIME_EVENT_SLOT_SITE_OFFSET) as usize;
            let site = sites.get(site_id);
            let site_layout = layout.runtime_event_site_layouts.get(site_id);
            let arg_count =
                read_payload_u64(slot_base + RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET) as usize;
            let arg_count = site
                .map(|site| arg_count.min(site.arg_widths.len()))
                .unwrap_or(0);
            let mut args = Vec::with_capacity(arg_count);
            if let Some(site_layout) = site_layout {
                for idx in 0..arg_count {
                    let Some(arg_layout) = site_layout.args.get(idx) else {
                        break;
                    };
                    let mut values = Vec::with_capacity(arg_layout.word_count);
                    let mut masks = Vec::with_capacity(arg_layout.word_count);
                    for word_idx in 0..arg_layout.word_count {
                        values.push(read_payload_u64(
                            slot_base
                                + RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                                + (arg_layout.value_word_offset + word_idx) * 8,
                        ));
                        masks.push(read_payload_u64(
                            slot_base
                                + RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                                + (arg_layout.mask_word_offset + word_idx) * 8,
                        ));
                    }
                    args.push(RuntimeEventArgValue {
                        float_precision: site
                            .and_then(|site| site.arg_float_precision.get(idx).copied())
                            .flatten(),
                        values,
                        masks,
                        width: site
                            .and_then(|site| site.arg_widths.get(idx).copied())
                            .unwrap_or(64),
                        signed: site
                            .and_then(|site| site.arg_signed.get(idx).copied())
                            .unwrap_or(false),
                        is_string: site
                            .and_then(|site| site.arg_is_string.get(idx).copied())
                            .unwrap_or(false),
                        handle: site
                            .and_then(|site| site.arg_is_handle.get(idx).copied())
                            .unwrap_or(false),
                    });
                }
            }

            let published_after = read_seq_u64(slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET);
            if published_after == RUNTIME_EVENT_WRITING || published_after != seq {
                break;
            }

            if site.is_some() {
                events.push(RawRuntimeEvent::Event { site_id, args });
            }
            *read_seq += 1;
        }

        events
    }

    fn drain_raw_runtime_events_from_buffer(
        buffer: &RuntimeEventBuffer,
        layout: &MemoryLayout,
        sites: &[RuntimeEventSite],
        read_seq: &mut u64,
    ) -> Vec<RawRuntimeEvent> {
        use std::sync::atomic::Ordering;

        collect_runtime_events(
            layout,
            sites,
            read_seq,
            buffer.byte_size(),
            |offset| buffer.read_u64(offset),
            |offset| buffer.load_atomic_u64(offset, Ordering::Acquire),
        )
    }

    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub(crate) fn runtime_event_write_seq_for_backend<B: SimBackend>(backend: &B) -> u64 {
        if let Some(buffer) = backend.runtime_event_buffer() {
            buffer.load_atomic_u64(0, std::sync::atomic::Ordering::Acquire)
        } else {
            let (pointer, size) = backend.runtime_event_buffer_as_ptr();
            if size < std::mem::size_of::<u64>() {
                return 0;
            }
            // Safety: the backend contract exposes `size` readable bytes.
            unsafe { std::ptr::read_volatile(pointer.cast::<u64>()) }
        }
    }

    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub(crate) fn collect_runtime_events_for_backend<B: SimBackend>(
        backend: &B,
        sites: &[RuntimeEventSite],
        read_seq: &mut u64,
        context: RuntimeFormatContext<'_>,
    ) -> Vec<RuntimeEvent> {
        let layout = backend.layout();
        let raw = if let Some(buffer) = backend.runtime_event_buffer() {
            collect_runtime_events(
                layout,
                sites,
                read_seq,
                buffer.byte_size(),
                |offset| buffer.read_u64(offset),
                |offset| buffer.load_atomic_u64(offset, std::sync::atomic::Ordering::Acquire),
            )
        } else {
            let (pointer, size) = backend.runtime_event_buffer_as_ptr();
            let read_u64 = |offset: usize| -> u64 {
                // Safety: `collect_runtime_events` bounds every access by `size`.
                unsafe { std::ptr::read_volatile(pointer.add(offset).cast::<u64>()) }
            };
            collect_runtime_events(layout, sites, read_seq, size, read_u64, read_u64)
        };
        raw.into_iter()
            .filter_map(|mut event| {
                event.resolve_handles(backend.heap());
                render_raw_runtime_event(event, sites, context)
            })
            .collect()
    }

    // ── Generic methods available for any backend ────────────────────────
    impl<B: SimBackend> Simulator<B> {
        pub(crate) fn decorate_runtime_error(&self, err: RuntimeErrorCode) -> RuntimeErrorCode {
            match err {
                RuntimeErrorCode::DetectedTrueLoopCode(code) => {
                    if self.comb_fatal_site(code).is_some() {
                        return RuntimeErrorCode::Runtime {
                            message: "Fatal assertion (runtime event record overwritten)"
                                .to_string(),
                            signals: Vec::new(),
                        };
                    }
                    let Some(info) = self.program.runtime_schema.runtime_errors.get(&code) else {
                        return RuntimeErrorCode::DetectedTrueLoop;
                    };
                    let signals = info
                        .signals
                        .iter()
                        .map(|addr| self.program.get_path(addr))
                        .collect::<Vec<_>>();
                    if info.message == "Detected True Loop" {
                        RuntimeErrorCode::DetectedTrueLoopAt { signals }
                    } else {
                        RuntimeErrorCode::Runtime {
                            message: info.message.clone(),
                            signals,
                        }
                    }
                }
                other => other,
            }
        }

        pub fn with_backend_and_program(
            backend: B,
            program: RuntimeProgram,
            warnings: Vec<CompilationWarning>,
        ) -> Self {
            let mut sim = Self::with_backend_and_program_for_simulation(backend, program, warnings);
            // Simulator does not execute initial kernels. Release combinational
            // time-zero gates that would otherwise wait forever for them.
            for process in &sim.program.runtime_schema.processes {
                let status = sim.backend.resolve_signal(&process.status);
                if sim.backend.get_as::<u8>(status) == 0 {
                    sim.backend
                        .set(status, celox_design::ProcessStatus::Done.code());
                }
            }
            sim
        }

        /// Construct the engine before a timed scheduler starts initial kernels.
        pub(crate) fn with_backend_and_program_for_simulation(
            backend: B,
            program: RuntimeProgram,
            warnings: Vec<CompilationWarning>,
        ) -> Self {
            let schedule = celox_design::schedule::ScheduleModel::derive(
                &program.design,
                &program.runtime_schema,
            );
            let (derived_ticks, derived_scratch) =
                celox_runtime::build_derived_tick_plans(&backend, &schedule);
            let event_edges =
                celox_runtime::build_event_edges(&backend, &schedule.events, &schedule.kinds);
            let (derived_units, derived_settle) =
                derived_edge_units(&backend, &program.runtime_schema, &derived_ticks);
            let comb_observer_reads = crate::comb_activation::ObserverReads::new(
                &backend,
                &program.runtime_schema.comb_observers,
            );
            let sites = &program.runtime_schema.runtime_event_sites;
            let deferred_sites = if sites.iter().any(|site| site.deferred.is_some()) {
                sites
                    .iter()
                    .map(|site| {
                        site.deferred
                            .map(|deferred| (deferred, site.kind.is_fatal()))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let sample_shadows: Vec<(SignalRef, SignalRef)> = program
                .runtime_schema
                .sample_shadows
                .iter()
                .map(|(variable, copy)| {
                    (
                        backend.resolve_signal(variable),
                        backend.resolve_signal(copy),
                    )
                })
                .collect();
            let mut comb_deferred_groups: Vec<(u32, usize)> = Vec::new();
            for observer in &program.runtime_schema.comb_observers {
                if matches!(
                    sites[observer.site_id as usize].deferred,
                    Some(celox_design::DeferredSite::Action { .. })
                ) && !comb_deferred_groups
                    .iter()
                    .any(|&(group, _)| group == observer.activation_group)
                {
                    comb_deferred_groups
                        .push((observer.activation_group, observer.site_id as usize));
                }
            }
            let mut sim = Self {
                backend,
                program,
                schedule,
                vcd_writer: None,
                sv_vcd: SvVcdControl::default(),
                dirty: false,
                declaration_phase: false,
                warnings,
                handle_slots: Default::default(),
                heap_safepoints_blocked: false,
                file_io: Default::default(),
                pending_host_events: Arc::new(std::sync::Mutex::new(PendingRuntimeEvents {
                    deferred: DeferredQueues {
                        sites: deferred_sites.clone(),
                        ..Default::default()
                    },
                    ..Default::default()
                })),
                deferred_assertions: !deferred_sites.is_empty(),
                comb_deferred_groups,
                timed: false,
                host_slot_work: !deferred_sites.is_empty() || !sample_shadows.is_empty(),
                sample_shadows,
                components: Default::default(),
                component_simulation: None,
                runtime_event_read_seq: Arc::new(AtomicU64::new(0)),
                runtime_event_drain_active: Arc::new(AtomicBool::new(false)),
                comb_observer_snapshots: Vec::new(),
                comb_observer_scratch: Vec::new(),
                comb_observer_reads,
                comb_observer_initial_eval: true,
                checkpoint_fingerprint: std::sync::OnceLock::new(),
                state_schema: std::sync::OnceLock::new(),
                diagnostics: crate::RuntimeDiagnostics::default(),
                plusargs: celox_runtime::plusargs::Plusargs::default(),
                tick_timing_ticks: 0,
                tick_timing_eval_apply_ns: 0,
                tick_timing_eval_comb_ns: 0,
                derived_ticks,
                derived_scratch: Some(Box::new(derived_scratch)),
                derived_units,
                derived_settle,
                frozen: false,
                host_watch: Default::default(),
                finish_scan_seq: 0,
                finish_sites: false,
                event_edges,
            };
            sim.initialize_copies();
            sim.comb_observer_snapshots = sim.snapshot_all_comb_observers();
            sim
        }

        fn record_tick_timing(&mut self, eval_apply_ns: u64, eval_comb_ns: u64) {
            let Some(every) = self.diagnostics.tick_timing_every else {
                return;
            };
            if every == 0 {
                return;
            }
            self.tick_timing_ticks = self.tick_timing_ticks.saturating_add(1);
            self.tick_timing_eval_apply_ns =
                self.tick_timing_eval_apply_ns.saturating_add(eval_apply_ns);
            self.tick_timing_eval_comb_ns =
                self.tick_timing_eval_comb_ns.saturating_add(eval_comb_ns);
            if self.tick_timing_ticks.is_multiple_of(every) {
                tracing::debug!(
                    "[tick-timing] ticks={} eval_apply_ms={:.3} eval_comb_ms={:.3} avg_apply_us={:.3} avg_comb_us={:.3}",
                    self.tick_timing_ticks,
                    self.tick_timing_eval_apply_ns as f64 / 1_000_000.0,
                    self.tick_timing_eval_comb_ns as f64 / 1_000_000.0,
                    self.tick_timing_eval_apply_ns as f64 / self.tick_timing_ticks as f64 / 1_000.0,
                    self.tick_timing_eval_comb_ns as f64 / self.tick_timing_ticks as f64 / 1_000.0,
                );
            }
        }

        pub(crate) fn apply_initial_values(&mut self) {
            let mut applied = false;
            let initial_memory_values = self.program.design.take_initial_state();
            for init in &initial_memory_values {
                applied = true;
                let signal = self.backend.resolve_signal(&init.address);
                match &init.data {
                    InitialMemoryData::Packed {
                        value,
                        mask,
                        written_mask,
                    } => {
                        let width_mask = if signal.width == 0 {
                            BigUint::default()
                        } else {
                            (BigUint::from(1u8) << signal.width) - BigUint::from(1u8)
                        };
                        let preserve_mask = &width_mask ^ (written_mask & &width_mask);
                        let (current_value, current_mask) = self.backend.get_four_state(signal);
                        let value = (current_value & &preserve_mask) | (value & written_mask);
                        let mask = (current_mask & &preserve_mask) | (mask & written_mask);
                        if self.backend.layout().four_state && signal.is_4state {
                            self.backend.set_four_state(signal, value, mask);
                        } else {
                            let known_mask = &width_mask ^ (&mask & &width_mask);
                            self.backend.set_wide(signal, value & known_mask);
                        }
                    }
                    InitialMemoryData::Writes(runs) => {
                        self.apply_initial_memory_writes(signal, runs);
                    }
                }
            }
            if applied {
                self.dirty = true;
            }
            self.program
                .design
                .restore_initial_state(initial_memory_values);
        }

        pub(crate) fn run_plusargs_request(
            &mut self,
            site: &celox_design::HostRequestSite<SignalRef>,
            _timestamp: u64,
        ) -> Result<(), celox_runtime::Halt> {
            let query_signal = site.args[0].id;
            let (query_value, query_mask) = self.backend.get_four_state(query_signal);
            if query_mask != BigUint::default() {
                return Err(celox_runtime::Halt::Error(
                    crate::RuntimeErrorCode::Runtime {
                        message: "plusargs query contains X or Z bits".into(),
                        signals: Vec::new(),
                    },
                ));
            }
            let query = query_value.to_bytes_be();
            let query = query
                .iter()
                .position(|byte| *byte != 0)
                .map_or(&[][..], |start| &query[start..]);
            let matched = if site.operation == 0 {
                self.plusargs.test(query)
            } else {
                let target = site.results[1].id;
                match self
                    .plusargs
                    .value(query, target.width)
                    .map_err(|message| {
                        celox_runtime::Halt::Error(crate::RuntimeErrorCode::Runtime {
                            message,
                            signals: Vec::new(),
                        })
                    })? {
                    Some((value, mask)) => {
                        if self.backend.layout().four_state && target.is_4state {
                            self.backend.set_four_state(target, value, mask);
                        } else {
                            let width_mask =
                                (BigUint::from(1u8) << target.width) - BigUint::from(1u8);
                            self.backend
                                .set_wide(target, value & (&width_mask ^ (mask & &width_mask)));
                        }
                        true
                    }
                    None => false,
                }
            };
            self.backend
                .set_wide(site.results[0].id, BigUint::from(u8::from(matched)));
            Ok(())
        }

        pub(crate) fn apply_testbench_memory_writes(
            &mut self,
            signal: SignalRef,
            runs: &[InitialMemoryWriteRun],
        ) {
            self.apply_initial_memory_writes(signal, runs);
            self.dirty = true;
        }

        fn apply_initial_memory_writes(
            &mut self,
            signal: SignalRef,
            runs: &[InitialMemoryWriteRun],
        ) {
            let value_byte_size = signal.width.div_ceil(8);
            let write_mask = self.backend.layout().four_state && signal.is_4state;
            // Initial writes precede the first full dump; no mutable view escapes.
            let (ptr, mem_len) = self.backend.memory_as_ptr();
            let ptr = ptr.cast_mut();
            let mem = unsafe { std::slice::from_raw_parts_mut(ptr, mem_len) };

            for run in runs {
                if run.bit_width == 0 {
                    continue;
                }
                if signal.array_layout.is_some() {
                    write_initial_run_to_plane(mem, signal, false, run, &run.value_bytes);
                    if write_mask {
                        write_initial_run_to_plane(mem, signal, true, run, &run.mask_bytes);
                    }
                    continue;
                }
                if run.bit_offset % 8 == 0 && run.bit_width % 8 == 0 {
                    let byte_offset = run.bit_offset / 8;
                    let byte_width = run.bit_width / 8;
                    let value_offset = signal.offset + byte_offset;
                    mem[value_offset..value_offset + byte_width]
                        .copy_from_slice(&run.value_bytes[..byte_width]);
                    if write_mask {
                        let mask_offset = signal.offset + value_byte_size + byte_offset;
                        mem[mask_offset..mask_offset + byte_width]
                            .copy_from_slice(&run.mask_bytes[..byte_width]);
                    }
                    continue;
                }

                write_bits_to_memory(
                    mem,
                    signal.offset * 8 + run.bit_offset,
                    run.bit_width,
                    &run.value_bytes,
                );
                if write_mask {
                    write_bits_to_memory(
                        mem,
                        (signal.offset + value_byte_size) * 8 + run.bit_offset,
                        run.bit_width,
                        &run.mask_bytes,
                    );
                }
            }
        }

        /// Drain runtime events, lazily evaluating dirty combinational logic.
        /// Fatal reports encountered during evaluation are returned as
        /// [`RuntimeEvent::Report`] records so the host can handle them.
        pub fn drain_runtime_events(&mut self) -> Vec<RuntimeEvent> {
            self.drain_runtime_events_with_context(RuntimeFormatContext::default())
        }

        pub fn drain_runtime_events_with_context(
            &mut self,
            ctx: RuntimeFormatContext<'_>,
        ) -> Vec<RuntimeEvent> {
            assert!(
                !self.runtime_event_drain_active.load(Ordering::Acquire),
                "cannot use Simulator::drain_runtime_events while a RuntimeEventDrain is active",
            );
            self.settle_for_runtime_event_drain();
            // A fatal action is drained as a fatal report.
            let _ = self.end_untimed_slot();
            self.collect_formatted_runtime_events(ctx)
        }

        /// Settle dirty combinational logic before its events are drained.
        pub(crate) fn settle_for_runtime_event_drain(&mut self) {
            if self.dirty {
                self.eval_comb_for_runtime_event_drain().unwrap();
            }
        }

        /// Without a scheduler, a host call is a time slot: end it.
        fn end_untimed_slot(&mut self) -> Result<(), RuntimeErrorCode> {
            if self.timed {
                return Ok(());
            }
            self.end_host_slot()
        }

        pub(crate) fn drain_runtime_events_deferred_with_context(
            &mut self,
            ctx: RuntimeFormatContext<'_>,
        ) -> Vec<RuntimeEvent> {
            assert!(
                !self.runtime_event_drain_active.load(Ordering::Acquire),
                "cannot use Simulator::drain_runtime_events while a RuntimeEventDrain is active",
            );
            if !self.program.runtime_schema.comb_observers.is_empty() && self.dirty {
                self.eval_comb_for_runtime_event_drain().unwrap();
            }
            self.collect_formatted_runtime_events(ctx)
        }

        /// Drain runtime events while combinational logic is settled, as
        /// after a step of a timed simulation.
        pub(crate) fn drain_settled_runtime_events_with_context(
            &mut self,
            ctx: RuntimeFormatContext<'_>,
        ) -> Vec<RuntimeEvent> {
            assert!(
                !self.runtime_event_drain_active.load(Ordering::Acquire),
                "cannot use Simulator::drain_runtime_events while a RuntimeEventDrain is active",
            );
            self.collect_formatted_runtime_events(ctx)
        }

        fn eval_comb_for_runtime_event_drain(&mut self) -> Result<(), RuntimeErrorCode> {
            let (result, _) = self.eval_comb_capturing_events();
            self.check_comb_backend_result(result)?;
            self.dirty = false;
            Ok(())
        }

        fn check_comb_backend_result(
            &self,
            result: Result<(), RuntimeErrorCode>,
        ) -> Result<(), RuntimeErrorCode> {
            // The status itself identifies a fatal capture even when its
            // record has been overwritten in the bounded event buffer.
            match result {
                Err(RuntimeErrorCode::DetectedTrueLoopCode(code))
                    if self.comb_fatal_site(code).is_some() =>
                {
                    Ok(())
                }
                other => other.map_err(|error| self.decorate_runtime_error(error)),
            }
        }

        fn comb_fatal_site(&self, code: i64) -> Option<&RuntimeEventSite> {
            let site = self
                .program
                .runtime_schema
                .runtime_event_sites
                .get(celox_runtime::comb_fatal_site(code)? as usize)?;
            site.kind.is_fatal().then_some(site)
        }

        fn collect_formatted_runtime_events(
            &mut self,
            ctx: RuntimeFormatContext<'_>,
        ) -> Vec<RuntimeEvent> {
            let queue = Arc::clone(&self.pending_host_events);
            let mut pending = queue.lock().unwrap();
            pending.extend(
                self.collect_backend_runtime_events(),
                self.backend.layout().runtime_event_capacity,
            );
            let events = pending.drain();
            drop(pending);
            events
                .into_iter()
                .filter_map(|raw| {
                    render_raw_runtime_event(
                        raw,
                        &self.program.runtime_schema.runtime_event_sites,
                        ctx,
                    )
                })
                .collect()
        }

        fn queue_host_runtime_event(
            &mut self,
            site: RuntimeEventSite,
            args: Vec<RuntimeEventArgValue>,
        ) {
            let queue = Arc::clone(&self.pending_host_events);
            let mut pending = queue.lock().unwrap();
            let capacity = self.backend.layout().runtime_event_capacity;
            pending.extend(self.collect_backend_runtime_events(), capacity);
            pending.push(
                RawRuntimeEvent::Host {
                    site: Box::new(site),
                    args,
                },
                capacity,
            );
        }

        /// The runtime events [`Self::drain_runtime_events`] would return
        /// next, without taking them.
        pub(crate) fn peek_runtime_events(&mut self) -> Vec<RuntimeEvent> {
            self.stash_runtime_events();
            let raw = self.pending_host_events.lock().unwrap().clone().drain();
            raw.into_iter()
                .filter_map(|raw| {
                    render_raw_runtime_event(
                        raw,
                        &self.program.runtime_schema.runtime_event_sites,
                        RuntimeFormatContext::default(),
                    )
                })
                .collect()
        }

        /// Make the design state, the host state design code observes and
        /// the undrained runtime events those of `other`, a simulator of the
        /// same design. Host state outside the memory image, such as open
        /// files, is not copied.
        pub(crate) fn mirror(&mut self, other: &mut Self) {
            other.stash_runtime_events();
            self.stash_runtime_events();
            let pending = other.pending_host_events.lock().unwrap().clone();
            *self.pending_host_events.lock().unwrap() = pending;
            let fingerprint = self.state_fingerprint();
            crate::simulator::StateImage::capture(&other.backend, other.state_fingerprint())
                .restore_into(&mut self.backend, fingerprint)
                .expect("a simulator of the same design");
            self.frozen = other.frozen;
            self.dirty = other.dirty;
            self.declaration_phase = other.declaration_phase;
            self.comb_observer_snapshots
                .clone_from(&other.comb_observer_snapshots);
            self.comb_observer_initial_eval = other.comb_observer_initial_eval;
            self.host_watch.set_log(other.host_watch.log().to_vec());
        }

        /// A host call without a scheduler is a time slot, which is over:
        /// deferred assertions mature, then the sampled copies are made.
        #[inline]
        fn end_host_slot(&mut self) -> Result<(), RuntimeErrorCode> {
            if !self.host_slot_work {
                return Ok(());
            }
            self.finish_host_slot()
        }

        #[cold]
        #[inline(never)]
        fn finish_host_slot(&mut self) -> Result<(), RuntimeErrorCode> {
            self.mature_deferred_assertions()?;
            self.sample_values();
            Ok(())
        }

        /// Before the first time slot, start the `$past` histories and the
        /// sampled copies with the values of their variables, the default
        /// sampled values (IEEE 1800-2023 16.5.1).
        pub(crate) fn initialize_copies(&mut self) {
            if !self.host_slot_work && self.program.runtime_schema.initial_copies.is_empty() {
                return;
            }
            for (variable, copy) in self.program.runtime_schema.initial_copies.clone() {
                let variable = self.backend.resolve_signal(&variable);
                let copy = self.backend.resolve_signal(&copy);
                celox_runtime::copy_signal(&mut self.backend, variable, copy);
            }
            self.sample_values();
        }

        /// Copy each sampled variable into its copy (IEEE 1800-2023
        /// 16.5.1).
        pub(crate) fn sample_values(&mut self) {
            for &(variable, copy) in &self.sample_shadows {
                celox_runtime::copy_signal(&mut self.backend, variable, copy);
            }
        }

        /// The time slot ends: the actions of deferred assertions that their
        /// process did not flush mature, `#0` before `final` (IEEE 1800-2023
        /// 16.4). A fatal one ends the simulation with an error.
        pub(crate) fn mature_deferred_assertions(&mut self) -> Result<(), RuntimeErrorCode> {
            if !self.deferred_assertions {
                return Ok(());
            }
            self.stash_runtime_events();
            let capacity = self.backend.layout().runtime_event_capacity;
            let fatal = self.pending_host_events.lock().unwrap().mature(capacity);
            if let Some(RawRuntimeEvent::Missed { count }) = fatal {
                return Err(RuntimeErrorCode::Runtime {
                    message: format!(
                        "missed {count} runtime events, which may have held a deferred $fatal"
                    ),
                    signals: Vec::new(),
                });
            }
            let Some(RuntimeEvent::Report(report)) = fatal.and_then(|raw| {
                render_raw_runtime_event(
                    raw,
                    &self.program.runtime_schema.runtime_event_sites,
                    RuntimeFormatContext::default(),
                )
            }) else {
                return Ok(());
            };
            Err(RuntimeErrorCode::Runtime {
                message: message_diagnostic(&report.message),
                signals: Vec::new(),
            })
        }

        /// Move the events in the backend's buffer to the queue that
        /// [`Self::drain_runtime_events`] reads.
        pub(crate) fn stash_runtime_events(&mut self) {
            let events = self.collect_backend_runtime_events();
            let capacity = self.backend.layout().runtime_event_capacity;
            self.pending_host_events
                .lock()
                .unwrap()
                .extend(events, capacity);
        }

        fn collect_backend_runtime_events(&mut self) -> Vec<RawRuntimeEvent> {
            // String arguments are read now, while the heap still holds them.
            let mut events = self.collect_backend_raw_runtime_events();
            for event in &mut events {
                event.resolve_handles(self.backend.heap());
            }
            events
        }

        fn collect_backend_raw_runtime_events(&mut self) -> Vec<RawRuntimeEvent> {
            let layout = self.backend.layout();
            let mut read_seq = self.runtime_event_read_seq.load(Ordering::Acquire);
            if let Some(buffer) = self.backend.runtime_event_buffer() {
                let events = collect_runtime_events(
                    layout,
                    &self.program.runtime_schema.runtime_event_sites,
                    &mut read_seq,
                    buffer.byte_size(),
                    |offset| buffer.read_u64(offset),
                    |offset| buffer.load_atomic_u64(offset, std::sync::atomic::Ordering::Acquire),
                );
                self.runtime_event_read_seq
                    .store(read_seq, Ordering::Release);
                events
            } else {
                let (ptr, size) = self.backend.runtime_event_buffer_as_ptr();
                let read_u64 = |offset: usize| -> u64 {
                    unsafe { std::ptr::read_volatile(ptr.add(offset) as *const u64) }
                };
                let events = collect_runtime_events(
                    layout,
                    &self.program.runtime_schema.runtime_event_sites,
                    &mut read_seq,
                    size,
                    read_u64,
                    read_u64,
                );
                self.runtime_event_read_seq
                    .store(read_seq, Ordering::Release);
                events
            }
        }

        /// Whether the design wrote a `$finish` record since the last call.
        pub(crate) fn take_runtime_finish(&mut self) -> bool {
            if !self.finish_sites {
                return false;
            }
            let write_seq = self.runtime_event_write_seq();
            if write_seq == self.finish_scan_seq {
                return false;
            }
            let events = self.peek_backend_runtime_events_from(self.finish_scan_seq);
            self.finish_scan_seq = write_seq;
            let sites = &self.program.runtime_schema.runtime_event_sites;
            events.iter().any(|event| {
                matches!(
                    event,
                    RawRuntimeEvent::Event { site_id, .. }
                        if sites.get(*site_id).is_some_and(|site| {
                            matches!(site.kind, RuntimeEventKind::Finish)
                        })
                )
            })
        }

        /// Start looking for `$finish` records at the current sequence.
        pub(crate) fn skip_runtime_finish(&mut self) {
            if self.finish_sites {
                self.finish_scan_seq = self.runtime_event_write_seq();
            }
        }

        fn runtime_event_write_seq(&self) -> u64 {
            if let Some(buffer) = self.backend.runtime_event_buffer() {
                buffer.load_atomic_u64(0, std::sync::atomic::Ordering::Acquire)
            } else {
                let (ptr, _size) = self.backend.runtime_event_buffer_as_ptr();
                unsafe { std::ptr::read_volatile(ptr as *const u64) }
            }
        }

        fn peek_backend_runtime_events_from(&self, read_seq: u64) -> Vec<RawRuntimeEvent> {
            let mut read_seq = read_seq;
            let layout = self.backend.layout();
            if let Some(buffer) = self.backend.runtime_event_buffer() {
                collect_runtime_events(
                    layout,
                    &self.program.runtime_schema.runtime_event_sites,
                    &mut read_seq,
                    buffer.byte_size(),
                    |offset| buffer.read_u64(offset),
                    |offset| buffer.load_atomic_u64(offset, std::sync::atomic::Ordering::Acquire),
                )
            } else {
                let (ptr, size) = self.backend.runtime_event_buffer_as_ptr();
                let read_u64 = |offset: usize| -> u64 {
                    unsafe { std::ptr::read_volatile(ptr.add(offset) as *const u64) }
                };
                collect_runtime_events(
                    layout,
                    &self.program.runtime_schema.runtime_event_sites,
                    &mut read_seq,
                    size,
                    read_u64,
                    read_u64,
                )
            }
        }

        pub fn runtime_event_drain(&mut self) -> Option<RuntimeEventDrain> {
            let buffer = self.backend.runtime_event_buffer()?;
            self.runtime_event_drain_active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .ok()?;
            Some(RuntimeEventDrain {
                buffer,
                layout: self.backend.layout().clone(),
                sites: self.program.runtime_schema.runtime_event_sites.clone(),
                pending_host_events: Arc::clone(&self.pending_host_events),
                read_seq: self.runtime_event_read_seq.load(Ordering::Acquire),
                shared_read_seq: Arc::clone(&self.runtime_event_read_seq),
                active: Arc::clone(&self.runtime_event_drain_active),
            })
        }

        /// Returns a reference to the compiled SIR program.
        pub fn program(&self) -> &RuntimeProgram {
            &self.program
        }

        /// Returns a reference to the backend (for signal/event resolution).
        pub fn backend_ref(&self) -> &B {
            &self.backend
        }

        /// Returns warnings emitted during compilation.
        pub fn warnings(&self) -> &[CompilationWarning] {
            &self.warnings
        }

        /// Consume the simulator and return its compilation diagnostics.
        pub fn into_warnings(self) -> Vec<CompilationWarning> {
            self.warnings
        }

        /// Captures the current state of all signals and writes them to the VCD file.
        ///
        /// # Panics
        ///
        /// Panics where [`Self::try_dump`] returns an error.
        pub fn dump(&mut self, timestamp: u64) {
            if let Err(error) = self.try_dump(timestamp) {
                panic!("VCD dump at {timestamp} failed: {error}");
            }
        }

        /// Captures the current state of all signals and writes them to the
        /// VCD file. Fails if combinational evaluation or writing fails, or
        /// if `timestamp` is earlier than the last dumped timestamp.
        pub fn try_dump(&mut self, timestamp: u64) -> Result<(), DumpError> {
            if self.frozen {
                return Ok(());
            }
            if self.dirty {
                self.eval_comb_checked().map_err(DumpError::Runtime)?;
                self.dirty = false;
            }
            let component_traces = self.vcd_external_values();
            if let Some(ref mut writer) = self.vcd_writer {
                writer
                    .dump_backend(timestamp, &mut self.backend, &component_traces)
                    .map_err(DumpError::Io)?;
            }
            Ok(())
        }

        /// Dump unless `timestamp` precedes what the VCD file already
        /// records, as after `run_until` to an earlier time.
        pub(crate) fn dump_unless_rewound(&mut self, timestamp: u64) {
            let rewound = self
                .vcd_writer
                .as_ref()
                .and_then(|writer| writer.last_timestamp())
                .is_some_and(|last| timestamp < last);
            if !rewound {
                self.dump(timestamp);
            }
        }

        pub(crate) fn try_dump_unless_rewound(&mut self, timestamp: u64) -> Result<(), DumpError> {
            let rewound = self
                .vcd_writer
                .as_ref()
                .and_then(|writer| writer.last_timestamp())
                .is_some_and(|last| timestamp < last);
            if rewound {
                Ok(())
            } else {
                self.try_dump(timestamp)
            }
        }

        /// Reject a return to `time` if the VCD file has already passed it.
        pub(crate) fn check_vcd_rewind(&self, time: u64) -> Result<(), CheckpointError> {
            match self
                .vcd_writer
                .as_ref()
                .and_then(|writer| writer.last_timestamp())
            {
                Some(last_dumped) if time < last_dumped => {
                    Err(CheckpointError::VcdRewind { time, last_dumped })
                }
                _ => Ok(()),
            }
        }

        /// Finish the current VCD file and continue the waveform in a new
        /// file at `path`, whose timestamps start over. Use it to record a
        /// simulation rewound by a restore. Fails if the simulator was built
        /// without VCD output.
        pub fn switch_vcd(&mut self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
            let writer = self.vcd_writer.as_mut().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "VCD output was not enabled when the simulator was built",
                )
            })?;
            writer.restart(path)
        }

        /// Make buffered waveform output visible and report write errors.
        pub fn flush_vcd(&mut self) -> std::io::Result<()> {
            if let Some(writer) = self.vcd_writer.as_mut() {
                writer.flush()?;
            }
            Ok(())
        }

        pub fn vcd_statistics(&self) -> Option<celox_runtime::VcdStatistics> {
            self.vcd_writer.as_ref().map(crate::VcdWriter::statistics)
        }

        /// Sets a signal value and marks combinational logic as dirty.
        /// Copies the value's little-endian bytes, zero-extending shorter
        /// values and truncating bits beyond the signal width. Integer type
        /// size does not need to match the signal storage size.
        pub fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
            self.backend.set(signal, val);
            self.dirty = true;
            self.settle_dirty_for_runtime_event_drain();
        }

        /// Sets a wide signal value and marks combinational logic as dirty.
        pub fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
            self.backend.set_wide(signal, val);
            self.dirty = true;
            self.settle_dirty_for_runtime_event_drain();
        }

        /// Sets a four-state signal value and marks combinational logic as dirty.
        pub fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint) {
            self.backend.set_four_state(signal, val, mask);
            self.dirty = true;
            self.settle_dirty_for_runtime_event_drain();
        }

        /// Modifies internal state via a callback and marks combinational logic as dirty.
        pub fn modify<F>(&mut self, f: F) -> Result<(), RuntimeErrorCode>
        where
            F: FnOnce(&mut IOContext<B>),
        {
            if self.frozen {
                return Ok(());
            }
            let mut ctx = IOContext {
                backend: &mut self.backend,
                watch: &mut self.host_watch,
            };
            f(&mut ctx);
            self.dirty = true;
            if self.runtime_event_drain_active.load(Ordering::Acquire) {
                self.eval_comb_for_runtime_event_drain()?;
                self.end_untimed_slot()?;
            }
            Ok(())
        }

        pub(super) fn settle_dirty_for_runtime_event_drain(&mut self) {
            if self.runtime_event_drain_active.load(Ordering::Acquire) {
                self.eval_comb_for_runtime_event_drain().unwrap();
                // A fatal action is drained as a fatal report.
                let _ = self.end_untimed_slot();
            }
        }

        pub(crate) fn eval_comb_checked(&mut self) -> Result<(), RuntimeErrorCode> {
            if self.frozen || self.declaration_phase {
                return Ok(());
            }
            let (result, events) = self.eval_comb_capturing_events();
            let fatal_status = matches!(
                &result,
                Err(RuntimeErrorCode::DetectedTrueLoopCode(code)) if self.comb_fatal_site(*code).is_some()
            );
            // Check the original backend code before formatting fatal records:
            // another parallel lane may have reported a loop or internal error.
            self.check_comb_backend_result(result)?;
            if let Some(error) = self.fatal_comb_capture_error(&events) {
                return Err(error);
            }
            if fatal_status {
                return Err(RuntimeErrorCode::Runtime {
                    message: "Fatal assertion (runtime event record overwritten)".to_string(),
                    signals: Vec::new(),
                });
            }
            // Without a scheduler, a domain samples the values the logic
            // settled to before it, as at the end of the previous call.
            // Deferred actions mature only when the call ends, so that a
            // later settle in the call can still flush them.
            if !self.timed && self.host_slot_work {
                self.sample_values();
            }
            Ok(())
        }

        fn eval_comb_capturing_events(
            &mut self,
        ) -> (Result<(), RuntimeErrorCode>, Vec<RawRuntimeEvent>) {
            // Event-drain writes use this entry directly instead of the
            // checked facade. Leave observers and runtime events untouched.
            if self.declaration_phase {
                return (Ok(()), Vec::new());
            }
            if self.program.runtime_schema.runtime_event_sites.is_empty() {
                return (self.backend.eval_comb(), Vec::new());
            }
            let runtime_event_start_seq = self.runtime_event_write_seq();
            if self.program.runtime_schema.comb_observers.is_empty() {
                let result = self.backend.eval_comb();
                let events = self.peek_backend_runtime_events_from(runtime_event_start_seq);
                return (result, events);
            }

            let mut snapshots = std::mem::take(&mut self.comb_observer_scratch);
            self.comb_observer_reads
                .snapshot(&self.backend, &mut snapshots);
            let active_sites = crate::comb_activation::active_sites(
                &self.program.runtime_schema.comb_observers,
                self.program.runtime_schema.runtime_event_sites.len(),
                &self.comb_observer_reads,
                &snapshots,
                &self.comb_observer_snapshots,
                self.comb_observer_initial_eval,
            );
            if !self.comb_deferred_groups.is_empty() {
                self.flush_comb_deferred(&active_sites);
            }
            self.backend.set_comb_capture_event_enabled(&active_sites);
            let eval_result = self.backend.eval_comb();
            self.comb_observer_reads
                .snapshot(&self.backend, &mut snapshots);
            let runtime_events = self.peek_backend_runtime_events_from(runtime_event_start_seq);
            self.backend.set_comb_capture_event_enabled(&vec![
                false;
                self.program
                    .runtime_schema
                    .runtime_event_sites
                    .len()
            ]);
            self.comb_observer_scratch =
                std::mem::replace(&mut self.comb_observer_snapshots, snapshots);
            self.comb_observer_initial_eval = false;
            (eval_result, runtime_events)
        }

        /// The combinational processes that run again flush the actions of
        /// their deferred assertions, after the events issued before.
        #[cold]
        fn flush_comb_deferred(&mut self, active_sites: &[bool]) {
            self.stash_runtime_events();
            let mut pending = self.pending_host_events.lock().unwrap();
            for &(group, site) in &self.comb_deferred_groups {
                if active_sites[site] {
                    pending.flush(group);
                }
            }
        }

        pub(super) fn snapshot_all_comb_observers(
            &self,
        ) -> Vec<crate::comb_activation::ObserverSnapshot> {
            self.comb_observer_reads.take(&self.backend)
        }

        fn fatal_comb_capture_error(&self, events: &[RawRuntimeEvent]) -> Option<RuntimeErrorCode> {
            events.iter().find_map(|raw| {
                let RawRuntimeEvent::Event { site_id, args } = raw else {
                    return None;
                };
                let site = self
                    .program
                    .runtime_schema
                    .runtime_event_sites
                    .get(*site_id)?;
                // A deferred `$fatal` ends the simulation when it matures.
                if !site.kind.is_fatal() || site.deferred.is_some() {
                    return None;
                }
                Some(RuntimeErrorCode::Runtime {
                    message: message_diagnostic(&render_runtime_event_message(
                        site,
                        args,
                        RuntimeFormatContext::default(),
                    )),
                    signals: Vec::new(),
                })
            })
        }

        pub(crate) fn eval_apply_ff_at_checked(
            &mut self,
            event: B::Event,
        ) -> Result<(), RuntimeErrorCode> {
            self.backend
                .eval_apply_ff_at(event)
                .map_err(|e| self.decorate_runtime_error(e))
        }

        pub(crate) fn eval_comb_apply_ff_at_checked(
            &mut self,
            event: B::Event,
        ) -> Result<(), RuntimeErrorCode> {
            self.backend
                .eval_comb_apply_ff_at(event)
                .map_err(|e| self.decorate_runtime_error(e))
        }

        pub(crate) fn eval_only_ff_at_checked(
            &mut self,
            event: B::Event,
        ) -> Result<(), RuntimeErrorCode> {
            self.backend
                .eval_only_ff_at(event)
                .map_err(|e| self.decorate_runtime_error(e))
        }

        pub(crate) fn apply_ff_at_checked(
            &mut self,
            event: B::Event,
        ) -> Result<(), RuntimeErrorCode> {
            self.backend
                .apply_ff_at(event)
                .map_err(|e| self.decorate_runtime_error(e))
        }

        /// One clock period of an event's signal: its active edge, then its
        /// inactive edge.
        ///
        /// A tick is defined as the SystemVerilog statements
        ///
        /// ```systemverilog
        /// s <= A; #H;
        /// s <= I; #L;
        /// ```
        ///
        /// on the event's signal `s`, where `A` is its active level (1 for a
        /// positive-edge clock or an active-high reset, 0 otherwise) and `I`
        /// the inactive one, with host operations taking place between
        /// periods. Every domain sensitive to either edge runs, including
        /// domains of the opposite edge and clocks derived from the signal,
        /// and the signal ends at the inactive level. A signal already at the
        /// active level takes no active edge. See the execution semantics
        /// guide for the host API as a whole.
        pub fn tick(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            if self.frozen {
                return Ok(());
            }
            let info = self.event_edges[event.id()];
            let active = info.active_edge();
            let inactive = active.opposite();
            if info.inactive_runs || !self.host_watch.is_empty() {
                self.active_edge(event)?;
                return self.inactive_edge(event);
            }
            if let Some(Some(_)) = self.derived_ticks.get(event.id()) {
                return self.tick_derived_quiet(event);
            }
            // Everything is read before the domain runs: right after its
            // kernel, loads of host tables can wait behind the kernel's
            // stores (4 KiB aliasing), which made a period cost almost two
            // domain runs. The inactive edge runs nothing and only stores.
            let observers = !self.program.runtime_schema.comb_observers.is_empty();
            if !info.level_read
                && !observers
                && celox_runtime::at_level(&self.backend, &info, inactive)
            {
                // Nothing the domain runs reads the level, which is the
                // inactive one before and after the period: both stores are
                // unobservable, and the period is one run of the domain.
                return self.fire(event);
            }
            let moved = celox_runtime::move_signal(
                &mut self.backend,
                &info,
                active,
                celox_runtime::set_level,
            );
            if let Some(old) = moved {
                if info.comb_read {
                    self.dirty = true;
                }
                if info.fires(old, active) {
                    self.fire(event)?;
                }
            }
            self.backend.set(info.signal, inactive.level());
            if info.comb_read || observers {
                self.dirty = true;
            }
            Ok(())
        }

        /// [`Self::tick`] of an event with derived domains whose inactive
        /// edge runs nothing: the active edge, then only the inactive level.
        #[inline(never)]
        fn tick_derived_quiet(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            self.active_edge(event)?;
            let info = self.event_edges[event.id()];
            if self.dirty {
                // Combinational logic did not settle, so the inactive edge
                // may still find a derived clock moved by the settle.
                return self.inactive_edge(event);
            }
            self.backend
                .set(info.signal, info.active_edge().opposite().level());
            if info.comb_read || !self.program.runtime_schema.comb_observers.is_empty() {
                self.dirty = true;
            }
            Ok(())
        }

        /// Run the event's domain as its active edge does, with the signal
        /// left as it is: the unit of the edge APIs.
        fn fire(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            let result = self.fire_unit(event);
            self.heap_safepoint();
            result.and(self.take_heap_fault())?;
            self.end_host_slot()
        }

        fn fire_unit(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            if let Some(Some(_)) = self.derived_ticks.get(event.id()) {
                return self.tick_derived(event);
            }
            let timing_enabled = self.diagnostics.tick_timing_every.is_some();
            let mut eval_comb_ns = 0u64;
            let mut eval_apply_ns = 0u64;
            if self.dirty {
                if timing_enabled {
                    let start = crate::timing::now();
                    self.eval_comb_checked()?;
                    eval_comb_ns = eval_comb_ns.saturating_add(start.elapsed().as_nanos() as u64);
                    let start = crate::timing::now();
                    self.eval_apply_ff_at_checked(event)?;
                    eval_apply_ns = eval_apply_ns.saturating_add(start.elapsed().as_nanos() as u64);
                } else {
                    self.eval_comb_checked()?;
                    self.eval_apply_ff_at_checked(event)?;
                }
                self.dirty = false;
            } else {
                if timing_enabled {
                    let start = crate::timing::now();
                    self.eval_apply_ff_at_checked(event)?;
                    eval_apply_ns = eval_apply_ns.saturating_add(start.elapsed().as_nanos() as u64);
                } else {
                    self.eval_apply_ff_at_checked(event)?;
                }
            }
            if timing_enabled {
                let start = crate::timing::now();
                self.eval_comb_checked()?;
                eval_comb_ns = eval_comb_ns.saturating_add(start.elapsed().as_nanos() as u64);
                self.record_tick_timing(eval_apply_ns, eval_comb_ns);
            } else {
                self.eval_comb_checked()?;
            }
            self.dirty = false;
            Ok(())
        }

        /// Move `event`'s signal through `edge` and run every domain
        /// sensitive to that transition, as a timed step does: the event's own
        /// domain when `edge` is its active edge, clocks computed
        /// combinationally from the signal that see their active edge (judged
        /// from the state before the edge, evaluating with the event's own
        /// domain), and, after the commit, gates the commit moves and domains
        /// clocked from state, transitively. The signal stays at its new
        /// level. An edge to the level the signal already has does nothing.
        ///
        /// An unknown (X) level counts as either level, as in SystemVerilog.
        pub fn edge(
            &mut self,
            event: B::Event,
            edge: celox_runtime::Edge,
        ) -> Result<(), RuntimeErrorCode> {
            // A timed simulation the design finished no longer changes.
            if self.frozen {
                return Ok(());
            }
            let info = self.event_edges[event.id()];
            if !matches!(self.derived_ticks.get(event.id()), Some(Some(_))) {
                if !info.can_fire(edge) && self.host_watch.is_empty() {
                    // An inactive edge runs nothing, whatever the level was:
                    // store the level without reading it first. A load of
                    // the signal right after the previous edge's kernel can
                    // wait behind the kernel's stores (4 KiB aliasing),
                    // which made a period cost almost two domain runs.
                    self.backend.set(info.signal, edge.level());
                    if info.comb_read || !self.program.runtime_schema.comb_observers.is_empty() {
                        self.dirty = true;
                    }
                    return Ok(());
                }
                // No derived domains: move the signal, then run the event's
                // domain.
                let write = watched_write(&mut self.host_watch);
                let Some(old) = celox_runtime::move_signal(&mut self.backend, &info, edge, write)
                else {
                    return Ok(());
                };
                if info.comb_read {
                    self.dirty = true;
                }
                if info.fires(old, edge) {
                    return self.fire(event);
                }
                if !self.program.runtime_schema.comb_observers.is_empty() {
                    self.dirty = true;
                }
                return Ok(());
            }
            if let Some(Some(unit)) = self.derived_units.get(event.id()).copied()
                && !self.finish_sites
                && self.backend.runs_derived_edge_units()
            {
                return self.edge_unit(info, edge, unit);
            }
            let plans = std::sync::Arc::clone(&self.derived_ticks);
            let plan = plans.get(event.id()).and_then(Option::as_ref);
            let mut scratch = self.derived_scratch.take().unwrap_or_default();
            let mut watch = std::mem::take(&mut self.host_watch);
            let dirty = self.dirty;
            let write = watched_write(&mut watch);
            let result =
                celox_runtime::run_edge(self, plan, info, &mut scratch, event, edge, dirty, write);
            self.host_watch = watch;
            self.derived_scratch = Some(scratch);
            self.note_edge(result)
        }

        /// [`Self::edge`] of an event with a generated derived-edge unit:
        /// move the signal, then run the unit of the edge, which judges the
        /// gated clocks, runs the domains and the cascade rounds, as the plan
        /// walk does.
        #[inline(never)]
        fn edge_unit(
            &mut self,
            info: celox_runtime::EventEdge,
            edge: celox_runtime::Edge,
            unit: DerivedEdgeUnit,
        ) -> Result<(), RuntimeErrorCode> {
            let mut watch = std::mem::take(&mut self.host_watch);
            let moved = celox_runtime::move_signal(
                &mut self.backend,
                &info,
                edge,
                watched_write(&mut watch),
            );
            self.host_watch = watch;
            let Some(old) = moved else {
                return Ok(());
            };
            let fires = info.fires(old, edge);
            if let Some(flag) = self.derived_settle {
                self.backend
                    .set(flag, u8::from(self.dirty || info.comb_read));
            }
            let index = match edge {
                celox_runtime::Edge::Rising => unit.rising,
                celox_runtime::Edge::Falling => unit.falling,
            };
            if let Err(error) = self.backend.run_process(index) {
                self.dirty = true;
                return Err(self.decorate_runtime_error(error));
            }
            // Without a gated clock and the event's own edge nothing ran, as
            // in the plan walk.
            let outcome = if unit.gated || fires {
                celox_runtime::EdgeOutcome::Settled
            } else if info.comb_read {
                celox_runtime::EdgeOutcome::Moved
            } else {
                celox_runtime::EdgeOutcome::Stored
            };
            self.note_edge(Ok(outcome))
        }

        /// Settle the outcome of [`celox_runtime::run_edge`] or
        /// [`celox_runtime::run_edges`], and end the time slot they ran.
        fn note_edge(
            &mut self,
            outcome: Result<celox_runtime::EdgeOutcome, celox_runtime::Halt>,
        ) -> Result<(), RuntimeErrorCode> {
            match outcome {
                Ok(celox_runtime::EdgeOutcome::Unchanged) => {}
                Ok(celox_runtime::EdgeOutcome::Moved) => self.dirty = true,
                // Combinational logic does not read the signal; observers
                // still see its new level at their next evaluation.
                Ok(celox_runtime::EdgeOutcome::Stored) => {
                    if !self.program.runtime_schema.comb_observers.is_empty() {
                        self.dirty = true;
                    }
                }
                Ok(celox_runtime::EdgeOutcome::Settled) => self.dirty = false,
                // An RTL `$finish` or `$stop` ends or suspends only a timed
                // simulation; an edge, as a tick, leaves it to the host. What
                // ran may not have settled.
                Err(celox_runtime::Halt::Finish | celox_runtime::Halt::Stop) => self.dirty = true,
                Err(celox_runtime::Halt::Error(error)) => return Err(error),
            }
            // The edges are a time slot, as a tick is.
            self.end_host_slot()
        }

        /// A rising edge of `event`'s signal; see [`Self::edge`].
        pub fn posedge(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            self.edge(event, celox_runtime::Edge::Rising)
        }

        /// A falling edge of `event`'s signal; see [`Self::edge`].
        pub fn negedge(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            self.edge(event, celox_runtime::Edge::Falling)
        }

        /// The event's active edge (rising for a positive-edge clock or an
        /// active-high reset, falling otherwise); see [`Self::edge`].
        pub fn active_edge(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            let edge = self.event_edges[event.id()].active_edge();
            self.edge(event, edge)
        }

        /// The edge opposite to the event's active edge; see [`Self::edge`].
        pub fn inactive_edge(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            let edge = match self.event_edges[event.id()].active_edge() {
                celox_runtime::Edge::Rising => celox_runtime::Edge::Falling,
                celox_runtime::Edge::Falling => celox_runtime::Edge::Rising,
            };
            self.edge(event, edge)
        }

        /// Move several event signals at one instant, as coincident edges:
        /// every domain sensitive to one of them evaluates against the state
        /// before the edges, then all of them commit, then the rounds of
        /// [`Self::edge`] follow.
        pub fn edges(
            &mut self,
            moves: &[(B::Event, celox_runtime::Edge)],
        ) -> Result<(), RuntimeErrorCode> {
            if self.frozen {
                return Ok(());
            }
            let plans = std::sync::Arc::clone(&self.derived_ticks);
            let edges = std::sync::Arc::clone(&self.event_edges);
            let mut scratch = self.derived_scratch.take().unwrap_or_default();
            let mut watch = std::mem::take(&mut self.host_watch);
            let dirty = self.dirty;
            let write = watched_write(&mut watch);
            let result =
                celox_runtime::run_edges(self, &plans, &edges, &mut scratch, moves, dirty, write);
            self.host_watch = watch;
            self.derived_scratch = Some(scratch);
            self.note_edge(result)
        }

        /// Per event ID, its signal and polarity, for hosts that drive the
        /// backend without this facade.
        pub fn event_edges(&self) -> celox_runtime::EventEdges {
            std::sync::Arc::clone(&self.event_edges)
        }

        /// Per event ID, the derived clock domains a tick of the event can
        /// trigger, for hosts that drive the backend without this facade.
        /// Empty when the design has no derived clocks.
        pub fn derived_tick_plans(&self) -> celox_runtime::DerivedTickPlans<B::Event> {
            std::sync::Arc::clone(&self.derived_ticks)
        }

        /// The domain run of an event whose edges can trigger derived
        /// domains, kept out of line so that the plain run stays small.
        #[inline(never)]
        fn tick_derived(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            let plans = std::sync::Arc::clone(&self.derived_ticks);
            let Some(Some(plan)) = plans.get(event.id()) else {
                unreachable!("tick_derived needs a plan");
            };
            let mut scratch = self.derived_scratch.take().unwrap_or_default();
            let dirty = self.dirty;
            let result = plan.tick(self, &mut scratch, dirty);
            self.derived_scratch = Some(scratch);
            if result.is_ok() {
                self.dirty = false;
            }
            result
        }

        /// [`Self::tick_events`] of coincident domains, one of which can
        /// trigger derived domains: the ticked domains evaluate and commit
        /// together, then the derived domains of all of them follow.
        #[cold]
        fn tick_events_derived(
            &mut self,
            domains: &[(B::Event, B::Event, B::Event)],
            unsplit: &[B::Event],
        ) -> Result<(), RuntimeErrorCode> {
            let sources: Vec<_> = domains
                .iter()
                .map(|&(event, eval_only, apply)| celox_runtime::EventDomain {
                    eval_apply: event,
                    eval_only: Some(eval_only),
                    apply: Some(apply),
                })
                .chain(unsplit.iter().map(|&event| celox_runtime::EventDomain {
                    eval_apply: event,
                    eval_only: None,
                    apply: None,
                }))
                .collect();
            let plans = std::sync::Arc::clone(&self.derived_ticks);
            let plan = celox_runtime::DerivedTickPlan::merge(
                sources,
                domains
                    .iter()
                    .map(|(event, _, _)| event)
                    .chain(unsplit)
                    .filter_map(|event| plans.get(event.id())?.as_ref()),
            );
            let mut scratch = self.derived_scratch.take().unwrap_or_default();
            let dirty = self.dirty;
            let result = plan.tick(self, &mut scratch, dirty);
            self.derived_scratch = Some(scratch);
            if result.is_ok() {
                self.dirty = false;
            }
            result
        }

        /// Tick several events at once: [`Self::tick`] with the edges of all
        /// their signals made together,
        ///
        /// ```systemverilog
        /// s1 <= A1; ... sk <= Ak; #H;
        /// s1 <= I1; ... sk <= Ik; #L;
        /// ```
        ///
        /// so every domain triggered by the coincident active edges samples
        /// the state settled before them, then all of them update. This is
        /// the behaviour of clocks that share an edge in hardware, such as a
        /// divided clock and its source, which sequential [`Self::tick`]
        /// calls cannot express: each of them would settle the design
        /// between the domains. Events of the same domain are applied once.
        /// A single event behaves like [`Self::tick`].
        pub fn tick_events(&mut self, events: &[B::Event]) -> Result<(), RuntimeErrorCode> {
            if self.frozen {
                return Ok(());
            }
            if let [event] = events {
                return self.tick(*event);
            }
            let only_active_edges = self.host_watch.is_empty()
                && events
                    .iter()
                    .all(|event| !self.event_edges[event.id()].inactive_runs);
            let derived = events
                .iter()
                .any(|event| matches!(self.derived_ticks.get(event.id()), Some(Some(_))));
            if !only_active_edges || derived {
                let edges = std::sync::Arc::clone(&self.event_edges);
                let moves = |edge: fn(celox_runtime::EventEdge) -> celox_runtime::Edge| {
                    events
                        .iter()
                        .map(|&event| (event, edge(edges[event.id()])))
                        .collect::<Vec<_>>()
                };
                let active = moves(|info| info.active_edge());
                self.edges(&active)?;
                if !only_active_edges || self.dirty {
                    let inactive = moves(|info| info.active_edge().opposite());
                    return self.edges(&inactive);
                }
                // The inactive edges run nothing and only store.
                for &event in events {
                    let info = self.event_edges[event.id()];
                    self.backend
                        .set(info.signal, info.active_edge().opposite().level());
                    if info.comb_read {
                        self.dirty = true;
                    }
                }
                if !self.program.runtime_schema.comb_observers.is_empty() {
                    self.dirty = true;
                }
                return Ok(());
            }
            // The inactive edges run nothing and only store.
            let mut fired = Vec::with_capacity(events.len());
            for &event in events {
                let info = self.event_edges[event.id()];
                let active = info.active_edge();
                if let Some(old) = celox_runtime::move_signal(
                    &mut self.backend,
                    &info,
                    active,
                    celox_runtime::set_level,
                ) {
                    if info.comb_read {
                        self.dirty = true;
                    }
                    if info.fires(old, active) {
                        fired.push(event);
                    }
                }
            }
            let result = self.tick_events_unit(&fired);
            self.heap_safepoint();
            result.and(self.take_heap_fault())?;
            self.end_host_slot()?;
            for &event in events {
                let info = self.event_edges[event.id()];
                self.backend
                    .set(info.signal, info.active_edge().opposite().level());
                if info.comb_read {
                    self.dirty = true;
                }
            }
            if !self.program.runtime_schema.comb_observers.is_empty() {
                self.dirty = true;
            }
            Ok(())
        }

        fn tick_events_unit(&mut self, events: &[B::Event]) -> Result<(), RuntimeErrorCode> {
            use celox_runtime::backend::EventHandle;
            // Deduplicate domains before requiring split kernels: a design
            // with a single domain has no split kernels, and aliases of one
            // domain are an ordinary tick.
            let mut unique: Vec<(crate::ir::AbsoluteAddr, B::Event)> =
                Vec::with_capacity(events.len());
            for event in events {
                let addr = self.backend.id_to_addr_slice()[event.id()];
                let canonical = self.schedule.events.canonical(addr);
                if unique.iter().all(|(existing, _)| *existing != canonical) {
                    unique.push((canonical, *event));
                }
            }
            // Split kernels exist for every active domain once a design has
            // more than one. A domain without them drives no FF, unless the
            // design has at most one active domain and no split kernels at
            // all; then ticking each event in turn is exact, because at most
            // one of them changes state.
            let mut domains = Vec::with_capacity(unique.len());
            let mut unsplit = Vec::new();
            for (canonical, event) in unique {
                match celox_runtime::EventDomain::split_kernels(&self.backend, &canonical) {
                    (Some(eval_only), Some(apply)) => domains.push((event, eval_only, apply)),
                    _ => unsplit.push(event),
                }
            }
            if !self.derived_ticks.is_empty()
                && domains.len() + unsplit.len() > 1
                && domains
                    .iter()
                    .map(|(event, _, _)| event)
                    .chain(&unsplit)
                    .any(|event| matches!(self.derived_ticks.get(event.id()), Some(Some(_))))
            {
                return self.tick_events_derived(&domains, &unsplit);
            }
            match domains.as_slice() {
                [] => {
                    for event in unsplit {
                        self.fire(event)?;
                    }
                    return Ok(());
                }
                [(event, _, _)] => return self.fire(*event),
                _ => {}
            }
            let timing_enabled = self.diagnostics.tick_timing_every.is_some();
            let mut eval_comb_ns = 0u64;
            let mut eval_apply_ns = 0u64;
            if self.dirty {
                let start = timing_enabled.then(crate::timing::now);
                self.eval_comb_checked()?;
                if let Some(start) = start {
                    eval_comb_ns = eval_comb_ns.saturating_add(start.elapsed().as_nanos() as u64);
                }
                self.dirty = false;
            }
            let start = timing_enabled.then(crate::timing::now);
            for (_, eval_only, _) in &domains {
                self.eval_only_ff_at_checked(*eval_only)?;
            }
            for (_, _, apply) in &domains {
                self.apply_ff_at_checked(*apply)?;
            }
            if let Some(start) = start {
                eval_apply_ns = eval_apply_ns.saturating_add(start.elapsed().as_nanos() as u64);
            }
            let start = timing_enabled.then(crate::timing::now);
            self.eval_comb_checked()?;
            if let Some(start) = start {
                eval_comb_ns = eval_comb_ns.saturating_add(start.elapsed().as_nanos() as u64);
                self.record_tick_timing(eval_apply_ns, eval_comb_ns);
            }
            self.dirty = false;
            Ok(())
        }

        /// Advance one event while deferring the post-edge combinational settle.
        /// Native testbenches use this between observable expressions so the
        /// preceding comb phase and this FF phase can share one compiled function.
        pub(crate) fn tick_deferred_comb(
            &mut self,
            event: B::Event,
        ) -> Result<(), RuntimeErrorCode> {
            if !self.program.runtime_schema.comb_observers.is_empty()
                || matches!(self.derived_ticks.get(event.id()), Some(Some(_)))
            {
                // Derived domains need the settled state to see their edges.
                return self.fire(event);
            }
            if self.dirty {
                self.eval_comb_apply_ff_at_checked(event)?;
            } else {
                self.eval_apply_ff_at_checked(event)?;
            }
            self.dirty = true;
            Ok(())
        }

        /// The edge facts of `event`.
        #[cfg(all(test, feature = "systemverilog"))]
        pub(crate) fn event_edge(&self, event: B::Event) -> celox_runtime::EventEdge {
            self.event_edges[event.id()]
        }

        /// Whether a period of `event`'s signal, a rise and a fall, runs
        /// domains only at its active edge: the event's own and those derived
        /// from it, while the inactive edge runs nothing.
        pub(crate) fn period_runs_only_active_edge(&self, event: B::Event) -> bool {
            !self.event_edges[event.id()].inactive_runs && self.host_watch.is_empty()
        }

        /// Advance a run of identical deferred-comb ticks. Native code may keep
        /// the loop inside the generated function, but must return after publishing
        /// a runtime event so host-side observation remains tick-accurate.
        pub(crate) fn tick_deferred_comb_many(
            &mut self,
            event: B::Event,
            count: u64,
        ) -> (u64, Result<(), RuntimeErrorCode>) {
            if count == 0 {
                return (0, Ok(()));
            }
            if !self.program.runtime_schema.comb_observers.is_empty()
                || !self.dirty
                || matches!(self.derived_ticks.get(event.id()), Some(Some(_)))
            {
                return (1, self.tick_deferred_comb(event));
            }
            let (completed, result) = self.backend.eval_comb_apply_ff_many_at(event, count);
            self.dirty = true;
            (
                completed,
                result.map_err(|error| self.decorate_runtime_error(error)),
            )
        }

        /// Resolves a signal path into a performance-optimized [`SignalRef`].
        /// This handle allows for direct memory access without `HashMap` lookups.
        ///
        /// # Panics
        ///
        /// Panics when the path does not resolve, including when the dead
        /// store elimination policy does not guarantee the signal; see
        /// [`Self::try_signal`].
        pub fn signal(&self, path: &str) -> SignalRef {
            self.try_signal(path)
                .unwrap_or_else(|error| panic!("cannot resolve signal: {error}"))
        }

        /// Resolve a port name to an event handle.
        pub fn event(&self, port: &str) -> B::Event {
            let addr = self.program.get_addr(&[], &[port]).unwrap();
            self.backend.resolve_event(&addr)
        }

        /// Try to resolve a signal path. Returns `Err` if the path is not found or ambiguous.
        pub fn try_signal(&self, path: &str) -> Result<SignalRef, crate::ir::AddrLookupError> {
            self.try_child_signal(&[], path)
        }

        /// Try to resolve a port name to an event handle.
        /// Returns an error if the path is missing, ambiguous, or has no registered event.
        pub fn try_event(&self, port: &str) -> Result<B::Event, crate::ir::AddrLookupError> {
            let addr = self.program.get_addr(&[], &[port])?;
            self.backend.resolve_event_opt(&addr).ok_or_else(|| {
                crate::ir::AddrLookupError::NotAnEvent {
                    path: port.to_string(),
                }
            })
        }

        /// Retrieves the current value as a fixed-size type without `BigUint` allocation.
        /// Lazily evaluates combinational logic if the state is dirty.
        pub fn get_as<T: Default + Copy>(&mut self, signal: SignalRef) -> T {
            if self.dirty {
                self.eval_comb_checked().unwrap();
                self.dirty = false;
            }
            self.backend.get_as(signal)
        }

        /// Retrieves the current value of a variable using a pre-resolved [`SignalRef`] handle.
        /// Lazily evaluates combinational logic if the state is dirty.
        pub fn get(&mut self, signal: SignalRef) -> BigUint {
            if self.dirty {
                self.eval_comb_checked().unwrap();
                self.dirty = false;
            }
            self.backend.get(signal)
        }

        /// Retrieves the current 4-state value (value, mask) of a variable using a [`SignalRef`] handle.
        /// Lazily evaluates combinational logic if the state is dirty.
        pub fn get_four_state(&mut self, signal: SignalRef) -> (BigUint, BigUint) {
            if self.dirty {
                self.eval_comb_checked().unwrap();
                self.dirty = false;
            }
            self.backend.get_four_state(signal)
        }

        /// Directly execute combinational logic evaluation.
        pub fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
            self.eval_comb_checked()?;
            self.dirty = false;
            self.end_untimed_slot()
        }

        /// Returns a raw pointer to the backend memory and its total size in bytes.
        pub fn memory_as_ptr(&self) -> (*const u8, usize) {
            self.backend.memory_as_ptr()
        }

        /// Returns a mutable raw pointer to the backend memory and its total size in bytes.
        pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
            self.backend.memory_as_mut_ptr()
        }

        /// Returns an opaque owner that keeps the backend memory allocation alive.
        pub fn memory_owner(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
            self.backend.memory_owner()
        }

        /// Returns the stable region size in bytes.
        pub fn stable_region_size(&self) -> usize {
            self.backend.stable_region_size()
        }

        /// Returns a reference to the memory layout.
        pub fn layout(&self) -> &MemoryLayout {
            self.backend.layout()
        }

        /// Take ownership of the VCD writer, if one was configured at build time.
        ///
        /// Builders create the writer themselves so that layout selection can
        /// account for VCD recording; callers building handles around a backend
        /// use this to keep tracing alive without rebuilding descriptors.
        pub fn take_vcd_writer(&mut self) -> Option<crate::VcdWriter> {
            self.vcd_writer.take()
        }

        /// Build VCD signal descriptors for all instances.
        ///
        /// The returned descriptors are self-contained (no IR references) and can
        /// be cached alongside [`SharedJitCode`] so that VCD works on cache-hit
        /// paths without the original [`RuntimeProgram`].
        pub fn build_vcd_descs(&self, four_state_mode: bool) -> Vec<crate::VcdSignalDesc> {
            let mut descs = Vec::new();
            let mut sorted_instances = self.program.design.instances().collect::<Vec<_>>();
            sorted_instances.sort_by_key(|instance| instance.id);

            for instance in sorted_instances {
                let scope = format!("{}", instance.id);

                let mut sorted_vars: Vec<_> = instance
                    .state_addresses()
                    .iter()
                    .filter_map(|address| self.program.design.variable(address))
                    .filter(|variable| instance.resolves_path_to(&variable.path, variable.address))
                    // A handle slot holds a heap reference, not a value to
                    // show; strings have no VCD representation yet.
                    .filter(|variable| {
                        !self
                            .program
                            .design
                            .state_objects
                            .get(&variable.address)
                            .is_some_and(|metadata| metadata.value_kind.is_handle())
                    })
                    .collect();
                sorted_vars.sort_by(|a, b| a.path.cmp(&b.path));

                for variable in sorted_vars {
                    let name = variable.path.join(".");
                    let signal = self.backend.resolve_signal(&variable.address);

                    descs.push(crate::VcdSignalDesc {
                        scope: scope.clone(),
                        name,
                        offset: signal.offset,
                        width: signal.width,
                        is_4state: four_state_mode && signal.is_4state,
                    });
                }
            }
            descs
        }

        /// Returns all ports of the top-level module with their resolved signal references.
        pub fn named_signals(&self) -> Vec<NamedSignal> {
            let top_instance = self
                .program
                .design
                .root_instance()
                .expect("top-level instance not found");
            self.build_signals_for_instance(top_instance.id)
        }

        /// Returns all signals for the instance at the given hierarchical path.
        ///
        /// The path is specified as a slice of `(instance_name, index)` pairs.
        /// Returns an empty `Vec` if the path does not exist.
        pub fn instance_signals(&self, instance_path: &[(&str, usize)]) -> Vec<NamedSignal> {
            let path: Vec<_> = instance_path
                .iter()
                .map(|(name, idx)| ((*name).to_string(), *idx))
                .collect();
            match self.program.design.instance_at_path(&InstancePath(path)) {
                Some(instance) => self.build_signals_for_instance(instance.id),
                None => Vec::new(),
            }
        }

        /// Builds the list of named signals for a given instance.
        ///
        /// Variables with ambiguous VarPaths (multiple scoped locals sharing the
        /// same name) are excluded — they cannot be addressed by path and would
        /// cause silent overwrites in name-keyed maps such as the layout JSON.
        fn build_signals_for_instance(
            &self,
            instance_id: crate::ir::InstanceId,
        ) -> Vec<NamedSignal> {
            let instance = self.program.design.instance(instance_id).unwrap();
            let in_root = self
                .program
                .design
                .root_instance()
                .is_some_and(|root| root.id == instance.id);

            let mut result = Vec::new();
            for address in instance.state_addresses() {
                let variable = self.program.design.variable(address).unwrap();
                // Skip variables whose VarPath is ambiguous (None in the path index).
                if !instance.resolves_path_to(&variable.path, *address) {
                    continue;
                }
                // Enumeration exposes the same handles path lookup would.
                if !self.observable_under_dead_store_policy(*address, in_root) {
                    continue;
                }
                let name = variable.path.join(".");
                let signal = self.backend.resolve_signal(address);

                // Resolve associated clock for reset signals
                let associated_clock = self
                    .program
                    .design
                    .events
                    .reset_clocks
                    .get(address)
                    .map(|clock_addr| self.program.get_path(clock_addr));

                result.push(NamedSignal {
                    name,
                    signal,
                    info: self.program.design.variable_info(address).unwrap(),
                    associated_clock,
                });
            }
            result
        }

        /// Returns all events (clock/reset signals) with their IDs and event references.
        pub fn named_events(&self) -> Vec<NamedEvent<B>> {
            let mut result = Vec::new();
            for (id, addr) in self.backend.id_to_addr_slice().iter().enumerate() {
                // A hidden event, such as the copy of a clock sensed at its
                // other edge, has no name to list.
                if !self.event_is_named(addr) {
                    continue;
                }
                let name = self.program.get_path(addr);
                if let Some(ev) = self.backend.resolve_event_opt(addr) {
                    result.push(NamedEvent {
                        name,
                        id,
                        event_ref: ev,
                    });
                }
            }
            result
        }

        /// Whether the event at `addr` is a variable a path names.
        fn event_is_named(&self, addr: &crate::ir::AbsoluteAddr) -> bool {
            self.program
                .design
                .instance(addr.instance_id)
                .is_some_and(|instance| {
                    self.program
                        .design
                        .variable(addr)
                        .is_some_and(|variable| instance.resolves_path_to(&variable.path, *addr))
                })
        }

        /// Triggers a clock/event by its numeric ID.
        ///
        /// # Errors
        ///
        /// Returns [`RuntimeErrorCode::NotAnEvent`] if `event_id` is not a
        /// known event ID, or a runtime error if evaluating the event fails.
        pub fn tick_by_id(&mut self, event_id: usize) -> Result<(), RuntimeErrorCode> {
            let event = self
                .backend
                .id_to_event_slice()
                .get(event_id)
                .copied()
                .ok_or_else(|| RuntimeErrorCode::NotAnEvent(format!("event_id={event_id}")))?;
            self.tick(event)
        }

        /// Triggers a clock/event N times by its numeric ID.
        /// Avoids repeated cross-boundary calls when used from FFI.
        /// The event ID is validated even when `count` is zero.
        ///
        /// # Errors
        ///
        /// Returns [`RuntimeErrorCode::NotAnEvent`] if `event_id` is not a
        /// known event ID, or a runtime error if evaluating an event fails.
        pub fn tick_by_id_n(
            &mut self,
            event_id: usize,
            count: u32,
        ) -> Result<(), RuntimeErrorCode> {
            let event = self
                .backend
                .id_to_event_slice()
                .get(event_id)
                .copied()
                .ok_or_else(|| RuntimeErrorCode::NotAnEvent(format!("event_id={event_id}")))?;
            for _ in 0..count {
                self.tick(event)?;
            }
            Ok(())
        }

        /// Resolves a signal inside a child instance.
        pub fn child_signal(&self, instance_path: &[(&str, usize)], var: &str) -> SignalRef {
            self.try_child_signal(instance_path, var)
                .unwrap_or_else(|error| panic!("cannot resolve signal: {error}"))
        }

        /// Try to resolve a signal inside a child instance.
        ///
        /// [`Self::named_signals`], [`Self::instance_signals`] and
        /// [`Self::named_hierarchy`] omit the same signals.
        ///
        /// Under a dead store elimination policy, only state the policy
        /// guarantees to keep resolves: ports the policy preserves, signals
        /// listed with `live_signal()`, and state no combinational logic
        /// writes. Anything else may have lost its stores, and a handle to it
        /// would only read memory that never updates.
        pub fn try_child_signal(
            &self,
            instance_path: &[(&str, usize)],
            var: &str,
        ) -> Result<SignalRef, crate::ir::AddrLookupError> {
            let addr = self.program.get_addr(instance_path, &[var])?;
            let in_root = self
                .program
                .design
                .root_instance()
                .is_some_and(|root| root.state_addresses().contains(&addr));
            if !self.observable_under_dead_store_policy(addr, in_root) {
                let path = instance_path
                    .iter()
                    .map(|(name, index)| format!("{name}[{index}]."))
                    .chain(std::iter::once(var.to_string()))
                    .collect::<String>();
                return Err(crate::ir::AddrLookupError::DeadStoreEliminated { path });
            }
            Ok(self.backend.resolve_signal(&addr))
        }

        /// Whether the dead store elimination policy guarantees that `addr`
        /// stays current. `in_root` says whether it belongs to the top
        /// instance, whose ports `PreserveTopPorts` keeps.
        fn observable_under_dead_store_policy(
            &self,
            addr: crate::ir::AbsoluteAddr,
            in_root: bool,
        ) -> bool {
            use crate::DeadStorePolicy;
            let schema = &self.program.runtime_schema;
            if schema.dead_store_policy == DeadStorePolicy::Off
                || !schema.comb_writes.contains(&addr)
                || schema.live_signals.contains(&addr)
                || schema.testbench_read_roots.contains(&addr)
            {
                return true;
            }
            let is_port = self
                .program
                .design
                .variable(&addr)
                .is_some_and(|variable| variable.var_kind.is_port());
            match schema.dead_store_policy {
                DeadStorePolicy::Off => true,
                DeadStorePolicy::PreserveListedSignals => false,
                DeadStorePolicy::PreserveTopPorts => in_root && is_port,
                DeadStorePolicy::PreserveAllPorts => is_port,
            }
        }

        /// Returns the full instance hierarchy starting from the top module.
        pub fn named_hierarchy(&self) -> InstanceHierarchy {
            self.build_hierarchy(&[])
        }

        fn build_hierarchy(&self, current_path: &[(String, usize)]) -> InstanceHierarchy {
            let instance = self
                .program
                .design
                .instance_at_path(&InstancePath(current_path.to_vec()))
                .expect("instance not found");
            let module_name = instance.module_name.clone();
            let indexed = instance
                .display_path
                .last()
                .is_some_and(|segment| segment.ends_with(']'));

            let signals = self.build_signals_for_instance(instance.id);

            // Find direct children: instance paths that extend current by exactly 1 segment
            let current_len = current_path.len();
            let mut children_map: crate::HashMap<String, Vec<(usize, InstanceHierarchy)>> =
                crate::HashMap::default();

            for child in self.program.design.instances() {
                if child.path.0.len() == current_len + 1 && child.path.0.starts_with(current_path) {
                    let (child_name, child_index) = &child.path.0[current_len];
                    let child_hierarchy = self.build_hierarchy(&child.path.0);
                    children_map
                        .entry(child_name.clone())
                        .or_default()
                        .push((*child_index, child_hierarchy));
                }
            }

            // Sort children by index within each group
            let mut children: Vec<(String, Vec<InstanceHierarchy>)> = children_map
                .into_iter()
                .map(|(name, mut instances)| {
                    instances.sort_by_key(|(idx, _)| *idx);
                    let sorted = instances.into_iter().map(|(_, h)| h).collect();
                    (name, sorted)
                })
                .collect();
            children.sort_by(|(a, _), (b, _)| a.cmp(b));

            InstanceHierarchy {
                module_name,
                index: current_path.last().map_or(0, |(_, index)| *index),
                indexed,
                signals,
                children,
            }
        }
    }

    // ── JitBackend-specific methods ──────────────────────────────────────
    impl Simulator {
        pub fn builder<'a>(code: &'a str, top: &'a str) -> SimulatorBuilder<'a, Simulator> {
            SimulatorBuilder::<Simulator>::new(code, top)
        }

        pub fn from_sources<'a>(
            sources: Vec<(&'a str, &'a std::path::Path)>,
            top: &'a str,
        ) -> SimulatorBuilder<'a, Simulator> {
            SimulatorBuilder::<Simulator>::from_sources(sources, top)
        }

        /// Low-level adapter hook for an elaborated external frontend artifact.
        ///
        /// Frontend crates should wrap this with a constructor named for their
        /// own artifact type instead of exposing
        /// [`celox_frontend_sdk::FrontendArtifact`] to applications.
        pub fn from_frontend(
            artifact: celox_frontend_sdk::FrontendArtifact,
        ) -> SimulatorBuilder<'static, Simulator> {
            SimulatorBuilder::<Simulator>::from_frontend(artifact)
        }

        /// Low-level adapter hook for a Veryl testbench around an external module.
        pub fn from_frontend_with_testbench<'a>(
            artifact: celox_frontend_sdk::FrontendArtifact,
            sources: Vec<(&'a str, &'a std::path::Path)>,
            top: &'a str,
        ) -> SimulatorBuilder<'a, Simulator> {
            SimulatorBuilder::<Simulator>::from_frontend_with_testbench(artifact, sources, top)
        }

        /// Build a simulator directly from SystemVerilog sources.
        #[cfg(feature = "systemverilog")]
        pub fn from_sv_sources<'a>(
            sources: Vec<(&'a str, &'a std::path::Path)>,
            top: &'a str,
        ) -> SimulatorBuilder<'a, Simulator> {
            SimulatorBuilder::<Simulator>::from_sv_sources(sources, top)
        }

        /// Build a mixed Veryl/SystemVerilog hierarchy with a top in either language.
        #[cfg(feature = "systemverilog")]
        pub fn from_mixed_sources<'a>(
            sources: Vec<(&'a str, &'a std::path::Path)>,
            sv_sources: Vec<(&'a str, &'a std::path::Path)>,
            top: &'a str,
        ) -> SimulatorBuilder<'a, Simulator> {
            SimulatorBuilder::<Simulator>::from_mixed_sources(sources, sv_sources, top)
        }
    }

    impl Simulator<JitBackend> {
        /// Returns the shared compiled JIT code, allowing it to be reused
        /// for creating additional simulator instances without recompilation.
        pub fn shared_code(&self) -> Arc<SharedJitCode> {
            self.backend.shared_code()
        }

        /// Consume the simulator and return the inner JIT backend.
        pub fn into_backend(self) -> JitBackend {
            self.backend
        }
    }

    impl Simulator<crate::backend::wasm_runtime::WasmBackend> {
        /// Consume the simulator and return the inner Wasmtime backend.
        pub fn into_backend(self) -> crate::backend::wasm_runtime::WasmBackend {
            self.backend
        }
    }

    impl Simulator<crate::backend::TieredBackend> {
        /// Whether this tiered simulation has adopted its compiled tier.
        ///
        /// False while background compilation is still running or after it
        /// failed (the interpreter then remains the permanent tier).
        pub fn is_compiled(&self) -> bool {
            self.backend.is_compiled()
        }

        /// Return deterministic tier lifecycle and evaluation measurements.
        ///
        /// The snapshot contains counts rather than ad-hoc wall-clock samples;
        /// use a statistical benchmark harness to measure elapsed time.
        pub fn tiered_execution_stats(&self) -> crate::TieredExecutionStats {
            self.backend.execution_stats()
        }

        /// Start opt-in wall-clock measurement from workload execution to
        /// promotion. Ordinary tiered simulation does not read the host clock.
        pub fn start_tiered_execution_timing(&mut self) {
            self.backend.start_execution_timing();
        }

        /// Stop tiered execution timing and return the promotion interval.
        pub fn finish_tiered_execution_timing(&mut self) -> Option<crate::TieredExecutionTiming> {
            self.backend.finish_execution_timing()
        }

        /// Why promotion has not happened yet, for diagnostics.
        pub fn promotion_error(&self) -> Option<&SimulatorError> {
            self.backend.promotion_error()
        }

        /// Request cancellation of background compilation.
        ///
        /// The simulation stays on the interpreter permanently and
        /// [`Simulator::promotion_error`] reports the cancellation. Returns
        /// whether a background compilation was still pending.
        pub fn cancel_background_compilation(&mut self) -> bool {
            self.backend.cancel_background_compilation()
        }

        /// Consume the simulator and return the inner tiered backend.
        pub fn into_backend(self) -> crate::backend::TieredBackend {
            self.backend
        }
    }

    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    impl Simulator<NativeBackend> {
        /// Returns the shared compiled native code, allowing it to be reused
        /// for creating additional simulator instances without recompilation.
        pub fn shared_code(&self) -> Arc<SharedNativeCode> {
            self.backend.shared_code()
        }

        /// Start measuring time spent inside generated native simulator functions.
        ///
        /// The measurement is disabled by default, so ordinary simulation does not
        /// read the host clock. Native tick loops are measured once per host-side
        /// batch rather than once per simulated tick.
        pub fn start_native_execution_timing(&mut self) {
            self.backend.start_execution_timing();
        }

        /// Stop native execution timing and return the accumulated measurement.
        pub fn finish_native_execution_timing(
            &mut self,
        ) -> Option<crate::native_backend::NativeExecutionTiming> {
            self.backend.finish_execution_timing()
        }

        /// Create a simulator from pre-compiled shared native code.
        pub fn from_shared(
            shared: Arc<SharedNativeCode>,
            program: crate::ir::OptimizedSir,
        ) -> Self {
            let backend = NativeBackend::from_shared(shared);
            let mut sim = Self::with_backend_and_program(backend, program.into_runtime(), vec![]);
            sim.apply_initial_values();
            sim
        }

        /// Consume the simulator and return the inner native backend.
        pub fn into_backend(self) -> NativeBackend {
            self.backend
        }
    }

    /// The generated units of one event's edges with its derived domains.
    #[derive(Clone, Copy, Debug)]
    struct DerivedEdgeUnit {
        rising: usize,
        falling: usize,
        /// The event gates derived clocks, so an edge always runs domains.
        gated: bool,
    }

    /// Per event ID, the generated derived-edge units of `schema`, and the flag
    /// they read to settle first.
    fn derived_edge_units<B: SimBackend>(
        backend: &B,
        schema: &celox_design::RuntimeSchema<crate::ir::AbsoluteAddr>,
        plans: &celox_runtime::DerivedTickPlans<B::Event>,
    ) -> (Arc<[Option<DerivedEdgeUnit>]>, Option<SignalRef>) {
        if schema.derived_edges.is_empty() {
            return (Arc::from([]), None);
        }
        let mut units = vec![None; backend.num_events()];
        for edges in &schema.derived_edges {
            let Some(event) = backend.resolve_event_opt(&edges.event) else {
                continue;
            };
            let Some(Some(plan)) = plans.get(event.id()) else {
                continue;
            };
            units[event.id()] = Some(DerivedEdgeUnit {
                rising: edges.rising,
                falling: edges.falling,
                gated: plan.lever_signals().next().is_some(),
            });
        }
        let flag = schema
            .scheduler
            .as_ref()
            .map(|scheduler| backend.resolve_signal(&scheduler.slots.dirty));
        (units.into(), flag)
    }

    /// The host's write of an event signal's level, observed by a timed
    /// simulation's waits and counters as [`IOContext`] writes are.
    fn watched_write<B: SimBackend>(
        watch: &mut celox_runtime::HostWatch,
    ) -> impl FnMut(&mut B, SignalRef, u8) + '_ {
        move |backend, signal, level| {
            if watch.is_empty() {
                backend.set(signal, level);
            } else {
                watch.write(backend, signal, |backend| backend.set(signal, level));
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const GATED_COUNTER: &str = "module Top (clk: input clock, en: input logic, count: output logic<8>, plain: output logic<8>) {
            let gated: '_ clock = clk & en;
            always_ff (gated) { count += 1; }
            always_ff (clk) { plain += 1; }
        }";

        #[test]
        fn edges_leave_a_finished_simulation_unchanged() {
            let mut sim = Simulator::builder(GATED_COUNTER, "Top").build().unwrap();
            let (clk, en) = (sim.event("clk"), sim.signal("en"));
            sim.modify(|io| io.set(en, 1u8)).unwrap();
            sim.posedge(clk).unwrap();
            let (count, plain) = (sim.signal("count"), sim.signal("plain"));
            assert_eq!(sim.get(count), BigUint::from(1u8));
            sim.negedge(clk).unwrap();
            sim.frozen = true;
            sim.posedge(clk).unwrap();
            sim.edges(&[(clk, celox_runtime::Edge::Rising)]).unwrap();
            sim.tick(clk).unwrap();
            let clk_signal = sim.signal("clk");
            assert_eq!(sim.get(clk_signal), BigUint::from(0u8));
            assert_eq!(sim.get(count), BigUint::from(1u8));
            assert_eq!(sim.get(plain), BigUint::from(1u8));
        }

        #[test]
        fn edges_write_the_clock_through_the_host_watch() {
            for design in [
                GATED_COUNTER,
                "module Top (clk: input clock, plain: output logic<8>) {
                always_ff (clk) { plain += 1; }
            }",
            ] {
                let mut sim = Simulator::builder(design, "Top").build().unwrap();
                let clk = sim.event("clk");
                let clk_signal = sim.signal("clk");
                let waited = [clk_signal.offset].into_iter().collect();
                sim.host_watch = celox_runtime::HostWatch::new([], waited);
                sim.posedge(clk).unwrap();
                sim.edges(&[(clk, celox_runtime::Edge::Falling)]).unwrap();
                // Already low: no write to observe.
                sim.negedge(clk).unwrap();
                let levels: Vec<_> = sim
                    .host_watch
                    .log()
                    .iter()
                    .map(|(signal, (value, _))| (signal.offset, value.clone()))
                    .collect();
                assert_eq!(
                    levels,
                    [
                        (clk_signal.offset, BigUint::from(1u8)),
                        (clk_signal.offset, BigUint::from(0u8)),
                    ],
                    "{design}"
                );
            }
        }

        #[test]
        fn edges_that_run_nothing_leave_comb_stale_only_when_comb_reads_the_clock() {
            for (seen, comb_read) in [("1'b1", false), ("clk", true)] {
                let source = format!(
                    "module Top (clk: input clock, count: output logic<8>, seen: output logic) {{
                        always_ff (clk) {{ count += 1; }}
                        assign seen = {seen};
                    }}"
                );
                let mut sim = Simulator::builder(&source, "Top").build().unwrap();
                let clk = sim.event("clk");
                sim.posedge(clk).unwrap();
                assert!(!sim.dirty);
                // Moving `clk` without running a domain leaves combinational
                // logic stale only when it reads `clk`.
                sim.negedge(clk).unwrap();
                assert_eq!(sim.dirty, comb_read, "{seen}");
                sim.tick(clk).unwrap();
                assert_eq!(sim.dirty, comb_read, "{seen}");
                let seen_signal = sim.signal("seen");
                let expected = if comb_read { 0u8 } else { 1 };
                assert_eq!(sim.get(seen_signal), BigUint::from(expected));
                let count = sim.signal("count");
                assert_eq!(sim.get(count), BigUint::from(2u8));
            }
        }

        #[test]
        fn comb_fatal_records_do_not_mask_unrelated_backend_errors() {
            let mut sim = Simulator::builder(
                r#"module Top {
                    always_comb {
                        $display("before");
                        $assert(1'd0, "fatal");
                    }
                }"#,
                "Top",
            )
            .build_interpreter()
            .unwrap();
            let (result, events) = sim.eval_comb_capturing_events();
            assert!(matches!(
                result,
                Err(RuntimeErrorCode::DetectedTrueLoopCode(-2))
            ));
            assert!(sim.check_comb_backend_result(result).is_ok());
            assert!(sim.fatal_comb_capture_error(&events).is_some());

            // These are the raw outcomes another parallel lane can report while
            // the fatal assertion's record is present in the shared buffer.
            for error in [
                RuntimeErrorCode::InternalError,
                RuntimeErrorCode::DetectedTrueLoop,
                RuntimeErrorCode::DetectedTrueLoopCode(1),
                RuntimeErrorCode::DetectedTrueLoopCode(2000),
                RuntimeErrorCode::Runtime {
                    message: "fatal".to_string(),
                    signals: Vec::new(),
                },
            ] {
                let expected = sim.decorate_runtime_error(error.clone());
                assert_eq!(sim.check_comb_backend_result(Err(error)), Err(expected));
            }
            // A fatal status remains identifiable after losing its record.
            assert!(
                sim.check_comb_backend_result(Err(RuntimeErrorCode::DetectedTrueLoopCode(
                    celox_runtime::comb_fatal_code(1)
                )),)
                    .is_ok()
            );
            // A display site, missing site, or internal failure is not fatal.
            for code in [
                celox_runtime::comb_fatal_code(0),
                celox_runtime::comb_fatal_code(99),
                i64::MIN,
            ] {
                assert!(
                    sim.check_comb_backend_result(Err(RuntimeErrorCode::DetectedTrueLoopCode(
                        code
                    )),)
                        .is_err()
                );
            }
        }

        #[test]
        fn comb_fatal_status_survives_overwritten_event_records() {
            use celox_state_layout::{
                RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
                RUNTIME_EVENT_SLOT_SEQ_OFFSET, RUNTIME_EVENT_SLOT_SITE_OFFSET,
            };
            let mut sim = Simulator::builder(
                r#"module Top {
                    always_comb {
                        $display("before");
                        $assert(1'd0, "fatal");
                    }
                }"#,
                "Top",
            )
            .build_interpreter()
            .unwrap();
            let (result, _) = sim.eval_comb_capturing_events();
            let buffer = sim.backend.runtime_event_buffer().unwrap();
            let layout = sim.backend.layout();
            let start = sim.runtime_event_write_seq();
            // Model the other lanes that finish publishing display records
            // after the fatal lane has returned. The simulator is idle here.
            for seq in start..start + layout.runtime_event_capacity as u64 {
                let slot = seq as usize & (layout.runtime_event_capacity - 1);
                let base = RUNTIME_EVENT_HEADER_SIZE + slot * layout.runtime_event_slot_size;
                unsafe {
                    let ptr = buffer.as_mut_ptr();
                    ptr.add(base + RUNTIME_EVENT_SLOT_SITE_OFFSET)
                        .cast::<u64>()
                        .write(0);
                    ptr.add(base + RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET)
                        .cast::<u64>()
                        .write(0);
                    (*ptr
                        .add(base + RUNTIME_EVENT_SLOT_SEQ_OFFSET)
                        .cast::<AtomicU64>())
                    .store(seq, Ordering::Release);
                    (*ptr.cast::<AtomicU64>()).store(seq + 1, Ordering::Release);
                }
            }
            let events = sim.peek_backend_runtime_events_from(0);
            assert!(matches!(
                events.first(),
                Some(RawRuntimeEvent::Missed { count: 2 })
            ));
            assert!(sim.fatal_comb_capture_error(&events).is_none());
            assert!(sim.check_comb_backend_result(result.clone()).is_ok());
            assert!(matches!(sim.decorate_runtime_error(result.unwrap_err()),
                RuntimeErrorCode::Runtime { message, .. } if message.contains("overwritten")));
            let drained = sim.drain_runtime_events();
            assert!(matches!(
                drained.first(),
                Some(RuntimeEvent::Missed { count: 2 })
            ));
            assert_eq!(
                drained.len(),
                1 + sim.backend.layout().runtime_event_capacity
            );
            assert!(sim.drain_runtime_events().is_empty());
        }
    }
}

#[cfg(feature = "host-runtime")]
pub use host::*;

/// Human-readable diagnostics are separate from the byte-exact event payload.
#[cfg(feature = "host-runtime")]
pub(crate) fn message_diagnostic(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.escape_ascii().to_string(),
    }
}
