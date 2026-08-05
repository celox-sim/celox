mod builder;
mod error;

pub use builder::compile_to_sir;
#[cfg(feature = "host-runtime")]
pub use builder::{DeadStorePolicy, SimulatorBuilder, SimulatorOptions};
#[cfg(feature = "systemverilog")]
pub use builder::{compile_mixed_to_sir, compile_sv_to_sir};
pub use error::render_diagnostic;
pub use error::{CodegenError, CompilationWarning, SimulatorError, SimulatorErrorKind};

#[cfg(feature = "host-runtime")]
mod host {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use crate::backend::RuntimeEventBuffer;
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", feature = "experimental-arm64-backend")
    ))]
    use crate::backend::native::{NativeBackend, SharedNativeCode};
    use crate::{
        IOContext, RuntimeErrorCode,
        backend::{JitBackend, MemoryLayout, SharedJitCode, SimBackend},
        ir::{
            InitialMemoryData, InitialMemoryWriteRun, InstancePath, RuntimeEventKind,
            RuntimeEventSite, RuntimeProgram, SignalRef, VariableInfo,
        },
    };
    use celox_testbench::{DisplayFormatArg, format_display_arg};
    use num_bigint::BigUint;

    /// Hierarchical instance tree with resolved signals.
    #[derive(Debug, Clone)]
    pub struct InstanceHierarchy {
        pub module_name: String,
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
    /// uses the custom native backend on x86-64 and opt-in AArch64, and Cranelift elsewhere.
    pub struct Simulator<B: SimBackend = crate::DefaultBackend> {
        pub(crate) backend: B,
        pub(crate) program: RuntimeProgram,
        pub(crate) vcd_writer: Option<crate::VcdWriter>,
        pub(crate) dirty: bool,
        pub(crate) warnings: Vec<CompilationWarning>,
        pub(crate) components: crate::component::ComponentRuntime,
        pub(crate) component_simulation: Option<celox_runtime::SimulationState<B>>,
        runtime_event_read_seq: Arc<AtomicU64>,
        runtime_event_drain_active: Arc<AtomicBool>,
        comb_observer_snapshots: Vec<Vec<(BigUint, BigUint)>>,
        comb_observer_initial_eval: bool,
        pub(crate) diagnostics: crate::RuntimeDiagnostics,
        tick_timing_ticks: u64,
        tick_timing_eval_apply_ns: u64,
        tick_timing_eval_comb_ns: u64,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RuntimeEvent {
        Display { message: String },
        AssertContinue { message: String },
        AssertFatal { message: String },
        Missed { count: u64 },
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
        read_seq: u64,
        shared_read_seq: Arc<AtomicU64>,
        active: Arc<AtomicBool>,
    }

    impl RuntimeEventDrain {
        pub fn drain(&mut self) -> Vec<RuntimeEvent> {
            self.drain_with_context(RuntimeFormatContext::default())
        }

        pub fn drain_with_context(&mut self, ctx: RuntimeFormatContext<'_>) -> Vec<RuntimeEvent> {
            let events = drain_raw_runtime_events_from_buffer(
                &self.buffer,
                &self.layout,
                &self.sites,
                &mut self.read_seq,
            );
            self.shared_read_seq.store(self.read_seq, Ordering::Release);
            events
                .into_iter()
                .filter_map(|raw| render_raw_runtime_event(raw, &self.sites, ctx))
                .collect()
        }
    }

    impl Drop for RuntimeEventDrain {
        fn drop(&mut self) {
            self.shared_read_seq.store(self.read_seq, Ordering::Release);
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
    }

    #[derive(Clone)]
    enum RawRuntimeEvent {
        Event {
            site_id: usize,
            args: Vec<RuntimeEventArgValue>,
        },
        Missed {
            count: u64,
        },
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

    fn mask_width(value: BigUint, width: usize) -> BigUint {
        if width == 0 {
            BigUint::from(0u8)
        } else {
            value & ((BigUint::from(1u8) << width) - BigUint::from(1u8))
        }
    }

    fn slice_biguint(value: &BigUint, lsb: usize, msb: usize) -> BigUint {
        if msb < lsb {
            return BigUint::from(0u8);
        }
        mask_width(value >> lsb, msb - lsb + 1)
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

    fn runtime_event_format_arg(arg: &RuntimeEventArgValue, spec: Option<char>) -> String {
        let value = runtime_event_words_to_biguint(&arg.values, arg.width);
        let mask = runtime_event_words_to_biguint(&arg.masks, arg.width);
        format_display_arg(
            &DisplayFormatArg {
                value: &value,
                mask: Some(&mask),
                width: arg.width,
                signed: arg.signed,
                is_string: arg.is_string,
            },
            spec,
        )
    }

    fn render_runtime_event_message(
        site: &RuntimeEventSite,
        args: &[RuntimeEventArgValue],
        ctx: RuntimeFormatContext<'_>,
    ) -> String {
        let Some(template) = site.template.as_deref() else {
            let default_spec = match site.kind {
                RuntimeEventKind::Display => 'd',
                RuntimeEventKind::AssertContinue | RuntimeEventKind::AssertFatal => {
                    if args.is_empty() {
                        return "assertion failed".to_string();
                    }
                    'x'
                }
            };
            return args
                .iter()
                .map(|arg| runtime_event_format_arg(arg, Some(default_spec)))
                .collect::<Vec<_>>()
                .join(" ");
        };
        let mut out = String::new();
        let mut arg_idx = 0usize;
        let mut chars = template.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '%' {
                out.push(ch);
                continue;
            }
            if matches!(chars.peek(), Some('%')) {
                chars.next();
                out.push('%');
                continue;
            }
            while matches!(chars.peek(), Some('0'..='9')) {
                chars.next();
            }
            let spec = chars.next().unwrap_or('d');
            match spec {
                'x' | 'h' | 'X' | 'H' | 'b' | 'B' | 'o' | 'O' | 'c' | 'C' | 's' | 'S' => {
                    let Some(arg) = args.get(arg_idx) else {
                        arg_idx += 1;
                        continue;
                    };
                    out.push_str(&runtime_event_format_arg(arg, Some(spec)));
                    arg_idx += 1;
                }
                'd' | 'D' | 'i' | 'I' => {
                    let Some(arg) = args.get(arg_idx) else {
                        arg_idx += 1;
                        continue;
                    };
                    out.push_str(&runtime_event_format_arg(arg, Some(spec)));
                    arg_idx += 1;
                }
                't' | 'T' => out.push_str(&ctx.tb_time.unwrap_or(0).to_string()),
                'm' | 'M' => out.push_str(ctx.scope.unwrap_or("<hierarchy>")),
                other => {
                    out.push('%');
                    out.push(other);
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
                let site = sites.get(site_id)?;
                let message = render_runtime_event_message(site, &args, ctx);
                Some(match site.kind {
                    RuntimeEventKind::Display => RuntimeEvent::Display { message },
                    RuntimeEventKind::AssertContinue => RuntimeEvent::AssertContinue { message },
                    RuntimeEventKind::AssertFatal => RuntimeEvent::AssertFatal { message },
                })
            }
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

    // ── Generic methods available for any backend ────────────────────────
    impl<B: SimBackend> Simulator<B> {
        fn decorate_runtime_error(&self, err: RuntimeErrorCode) -> RuntimeErrorCode {
            match err {
                RuntimeErrorCode::DetectedTrueLoopCode(code) => {
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
            let mut sim = Self {
                backend,
                program,
                vcd_writer: None,
                dirty: false,
                warnings,
                components: Default::default(),
                component_simulation: None,
                runtime_event_read_seq: Arc::new(AtomicU64::new(0)),
                runtime_event_drain_active: Arc::new(AtomicBool::new(false)),
                comb_observer_snapshots: Vec::new(),
                comb_observer_initial_eval: true,
                diagnostics: crate::RuntimeDiagnostics::default(),
                tick_timing_ticks: 0,
                tick_timing_eval_apply_ns: 0,
                tick_timing_eval_comb_ns: 0,
            };
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
            let initial_memory_values = std::mem::take(&mut self.program.design.initial_state);
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
            self.program.design.initial_state = initial_memory_values;
        }

        fn apply_initial_memory_writes(
            &mut self,
            signal: SignalRef,
            runs: &[InitialMemoryWriteRun],
        ) {
            let value_byte_size = signal.width.div_ceil(8);
            let write_mask = self.backend.layout().four_state && signal.is_4state;
            let (ptr, mem_len) = self.backend.memory_as_mut_ptr();
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
            if self.dirty {
                self.eval_comb_checked().unwrap();
                self.dirty = false;
            }
            self.collect_formatted_runtime_events(ctx)
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
                self.eval_comb_checked().unwrap();
                self.dirty = false;
            }
            self.collect_formatted_runtime_events(ctx)
        }

        fn collect_formatted_runtime_events(
            &mut self,
            ctx: RuntimeFormatContext<'_>,
        ) -> Vec<RuntimeEvent> {
            if self.runtime_event_read_seq.load(Ordering::Acquire) == self.runtime_event_write_seq()
            {
                return Vec::new();
            }
            self.collect_backend_runtime_events()
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

        fn collect_backend_runtime_events(&mut self) -> Vec<RawRuntimeEvent> {
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

        /// Captures the current state of all signals and writes them to the VCD file.
        pub fn dump(&mut self, timestamp: u64) {
            if self.dirty {
                self.eval_comb_checked().unwrap();
                self.dirty = false;
            }
            let component_traces = self.components.trace_values();
            if let Some(ref mut writer) = self.vcd_writer {
                let (ptr, size) = self.backend.memory_as_ptr();
                let memory = unsafe { std::slice::from_raw_parts(ptr, size) };
                writer
                    .dump_with_external(timestamp, memory, &component_traces)
                    .unwrap();
            }
        }

        /// Sets a signal value and marks combinational logic as dirty.
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
            let mut ctx = IOContext {
                backend: &mut self.backend,
            };
            f(&mut ctx);
            self.dirty = true;
            if self.runtime_event_drain_active.load(Ordering::Acquire) {
                self.eval_comb_checked()?;
                self.dirty = false;
            }
            Ok(())
        }

        fn settle_dirty_for_runtime_event_drain(&mut self) {
            if self.runtime_event_drain_active.load(Ordering::Acquire) {
                self.eval_comb_checked().unwrap();
                self.dirty = false;
            }
        }

        pub(crate) fn eval_comb_checked(&mut self) -> Result<(), RuntimeErrorCode> {
            if self.program.runtime_schema.runtime_event_sites.is_empty() {
                return self
                    .backend
                    .eval_comb()
                    .map_err(|e| self.decorate_runtime_error(e));
            }
            if self.program.runtime_schema.comb_observers.is_empty() {
                let runtime_event_start_seq = self.runtime_event_write_seq();
                let eval_result = self
                    .backend
                    .eval_comb()
                    .map_err(|e| self.decorate_runtime_error(e));
                let runtime_events = self.peek_backend_runtime_events_from(runtime_event_start_seq);
                if let Some(err) = self.fatal_comb_capture_error(&runtime_events) {
                    return Err(err);
                }
                return eval_result;
            }

            let before = self.snapshot_all_comb_observers();
            let active_before: Vec<bool> = before
                .iter()
                .zip(&self.comb_observer_snapshots)
                .map(|(now, prev)| now != prev)
                .collect();
            let mut active_sites =
                vec![false; self.program.runtime_schema.runtime_event_sites.len()];
            for (observer, is_active) in self
                .program
                .runtime_schema
                .comb_observers
                .iter()
                .zip(active_before.iter().copied())
            {
                if is_active || self.comb_observer_initial_eval {
                    let group = observer.activation_group;
                    for group_observer in &self.program.runtime_schema.comb_observers {
                        if group_observer.activation_group == group {
                            active_sites[group_observer.site_id as usize] = true;
                        }
                    }
                }
            }
            self.backend.set_comb_capture_event_enabled(&active_sites);
            let runtime_event_start_seq = self.runtime_event_write_seq();
            let eval_result = self
                .backend
                .eval_comb()
                .map_err(|e| self.decorate_runtime_error(e));
            let after = self.snapshot_all_comb_observers();
            let runtime_events = self.peek_backend_runtime_events_from(runtime_event_start_seq);
            let fatal_error = self.fatal_comb_capture_error(&runtime_events);
            self.backend.set_comb_capture_event_enabled(&vec![
                false;
                self.program
                    .runtime_schema
                    .runtime_event_sites
                    .len()
            ]);
            self.comb_observer_snapshots = after;
            self.comb_observer_initial_eval = false;
            if let Some(err) = fatal_error {
                return Err(err);
            }
            eval_result
        }

        fn snapshot_all_comb_observers(&self) -> Vec<Vec<(BigUint, BigUint)>> {
            self.program
                .runtime_schema
                .comb_observers
                .iter()
                .map(|observer| {
                    observer
                        .sensitivity
                        .iter()
                        .map(|atom| {
                            let signal = self.backend.resolve_signal(&atom.id);
                            let (value, mask) = if signal.is_4state {
                                self.backend.get_four_state(signal)
                            } else {
                                (self.backend.get(signal), BigUint::default())
                            };
                            (
                                slice_biguint(&value, atom.access.lsb, atom.access.msb),
                                slice_biguint(&mask, atom.access.lsb, atom.access.msb),
                            )
                        })
                        .collect()
                })
                .collect()
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
                if !matches!(site.kind, RuntimeEventKind::AssertFatal) {
                    return None;
                }
                Some(RuntimeErrorCode::Runtime {
                    message: render_runtime_event_message(
                        site,
                        args,
                        RuntimeFormatContext::default(),
                    ),
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

        /// Manually triggers a clock or event to process sequential logic.
        pub fn tick(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
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

        /// Advance one event while deferring the post-edge combinational settle.
        /// Native testbenches use this between observable expressions so the
        /// preceding comb phase and this FF phase can share one compiled function.
        pub(crate) fn tick_deferred_comb(
            &mut self,
            event: B::Event,
        ) -> Result<(), RuntimeErrorCode> {
            if !self.program.runtime_schema.comb_observers.is_empty() {
                return self.tick(event);
            }
            if self.dirty {
                self.eval_comb_apply_ff_at_checked(event)?;
            } else {
                self.eval_apply_ff_at_checked(event)?;
            }
            self.dirty = true;
            Ok(())
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
            if !self.program.runtime_schema.comb_observers.is_empty() || !self.dirty {
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
        pub fn signal(&self, path: &str) -> SignalRef {
            let addr = self.program.get_addr(&[], &[path]).unwrap();
            self.backend.resolve_signal(&addr)
        }

        /// Resolve a port name to an event handle.
        pub fn event(&self, port: &str) -> B::Event {
            let addr = self.program.get_addr(&[], &[port]).unwrap();
            self.backend.resolve_event(&addr)
        }

        /// Try to resolve a signal path. Returns `Err` if the path is not found or ambiguous.
        pub fn try_signal(&self, path: &str) -> Result<SignalRef, crate::ir::AddrLookupError> {
            let addr = self.program.get_addr(&[], &[path])?;
            Ok(self.backend.resolve_signal(&addr))
        }

        /// Try to resolve a port name to an event handle.
        pub fn try_event(&self, port: &str) -> Result<B::Event, crate::ir::AddrLookupError> {
            let addr = self.program.get_addr(&[], &[port])?;
            Ok(self.backend.resolve_event(&addr))
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
            Ok(())
        }

        /// Returns a raw pointer to the backend memory and its total size in bytes.
        pub fn memory_as_ptr(&self) -> (*const u8, usize) {
            self.backend.memory_as_ptr()
        }

        /// Returns a mutable raw pointer to the backend memory and its total size in bytes.
        pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
            self.backend.memory_as_mut_ptr()
        }

        /// Returns the stable region size in bytes.
        pub fn stable_region_size(&self) -> usize {
            self.backend.stable_region_size()
        }

        /// Returns a reference to the memory layout.
        pub fn layout(&self) -> &MemoryLayout {
            self.backend.layout()
        }

        /// Build VCD signal descriptors for all instances.
        ///
        /// The returned descriptors are self-contained (no IR references) and can
        /// be cached alongside [`SharedJitCode`] so that VCD works on cache-hit
        /// paths without the original [`RuntimeProgram`].
        pub fn build_vcd_descs(&self, four_state_mode: bool) -> Vec<crate::VcdSignalDesc> {
            let mut descs = Vec::new();
            let mut sorted_instances: Vec<_> =
                self.program.frontend.instance_module.iter().collect();
            sorted_instances.sort_by_key(|(id, _)| *id);

            for (instance_id, module_id) in sorted_instances {
                let variables = &self.program.frontend.module_variables[module_id];
                let path_index = &self.program.frontend.module_var_path_index[module_id];
                let scope = format!("{}", instance_id);

                let mut sorted_vars: Vec<_> = variables
                    .values()
                    .filter(|info| path_index.get(&info.path) != Some(&None))
                    .collect();
                sorted_vars.sort_by(|a, b| {
                    let name_a = a.path.to_string();
                    let name_b = b.path.to_string();
                    name_a.cmp(&name_b)
                });

                for info in sorted_vars {
                    let name = info
                        .path
                        .0
                        .iter()
                        .map(|s| {
                            veryl_parser::resource_table::get_str_value(*s)
                                .unwrap()
                                .to_string()
                        })
                        .collect::<Vec<_>>()
                        .join(".");

                    let addr = self
                        .program
                        .state_address_for_source(*instance_id, info.id)
                        .expect("frontend state projection is complete");
                    let signal = self.backend.resolve_signal(&addr);

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
            let top_instance_id = self
                .program
                .frontend
                .instance_ids
                .get(&InstancePath(vec![]))
                .expect("top-level instance not found");
            self.build_signals_for_instance(*top_instance_id)
        }

        /// Returns all signals for the instance at the given hierarchical path.
        ///
        /// The path is specified as a slice of `(instance_name, index)` pairs.
        /// Returns an empty `Vec` if the path does not exist.
        pub fn instance_signals(&self, instance_path: &[(&str, usize)]) -> Vec<NamedSignal> {
            let path_str_ids: Vec<_> = instance_path
                .iter()
                .map(|(name, idx)| (veryl_parser::resource_table::insert_str(name), *idx))
                .collect();
            match self
                .program
                .frontend
                .instance_ids
                .get(&InstancePath(path_str_ids))
            {
                Some(&instance_id) => self.build_signals_for_instance(instance_id),
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
            let module_id = &self.program.frontend.instance_module[&instance_id];
            let module_vars = &self.program.frontend.module_variables[module_id];
            let path_index = &self.program.frontend.module_var_path_index[module_id];

            let mut result = Vec::new();
            for info in module_vars.values() {
                // Skip variables whose VarPath is ambiguous (None in the path index).
                if path_index.get(&info.path) == Some(&None) {
                    continue;
                }
                let name = info
                    .path
                    .0
                    .iter()
                    .map(|s| {
                        veryl_parser::resource_table::get_str_value(*s)
                            .unwrap()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                let addr = self
                    .program
                    .state_address_for_source(instance_id, info.id)
                    .expect("frontend state projection is complete");
                let signal = self.backend.resolve_signal(&addr);

                // Resolve associated clock for reset signals
                let associated_clock = self
                    .program
                    .design
                    .events
                    .reset_clocks
                    .get(&addr)
                    .map(|clock_addr| self.program.get_path(clock_addr));

                result.push(NamedSignal {
                    name,
                    signal,
                    info: info.clone(),
                    associated_clock,
                });
            }
            result
        }

        /// Returns all events (clock/reset signals) with their IDs and event references.
        pub fn named_events(&self) -> Vec<NamedEvent<B>> {
            let mut result = Vec::new();
            for (id, addr) in self.backend.id_to_addr_slice().iter().enumerate() {
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

        /// Triggers a clock/event by its numeric ID.
        pub fn tick_by_id(&mut self, event_id: usize) -> Result<(), RuntimeErrorCode> {
            let event = self.backend.id_to_event_slice()[event_id];
            self.tick(event)
        }

        /// Triggers a clock/event N times by its numeric ID.
        /// Avoids repeated cross-boundary calls when used from FFI.
        pub fn tick_by_id_n(
            &mut self,
            event_id: usize,
            count: u32,
        ) -> Result<(), RuntimeErrorCode> {
            let event = self.backend.id_to_event_slice()[event_id];
            for _ in 0..count {
                self.tick(event)?;
            }
            Ok(())
        }

        /// Resolves a signal inside a child instance.
        pub fn child_signal(&self, instance_path: &[(&str, usize)], var: &str) -> SignalRef {
            let addr = self.program.get_addr(instance_path, &[var]).unwrap();
            self.backend.resolve_signal(&addr)
        }

        /// Try to resolve a signal inside a child instance.
        pub fn try_child_signal(
            &self,
            instance_path: &[(&str, usize)],
            var: &str,
        ) -> Result<SignalRef, crate::ir::AddrLookupError> {
            let addr = self.program.get_addr(instance_path, &[var])?;
            Ok(self.backend.resolve_signal(&addr))
        }

        /// Returns the full instance hierarchy starting from the top module.
        pub fn named_hierarchy(&self) -> InstanceHierarchy {
            self.build_hierarchy(&[])
        }

        fn build_hierarchy(
            &self,
            current_path: &[(veryl_parser::resource_table::StrId, usize)],
        ) -> InstanceHierarchy {
            let instance_id = self
                .program
                .frontend
                .instance_ids
                .get(&InstancePath(current_path.to_vec()))
                .expect("instance not found");
            let module_id = &self.program.frontend.instance_module[instance_id];
            let module_name = self
                .program
                .frontend
                .module_names
                .get(module_id)
                .and_then(|name| veryl_parser::resource_table::get_str_value(*name))
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}", module_id));

            let signals = self.build_signals_for_instance(*instance_id);

            // Find direct children: instance paths that extend current by exactly 1 segment
            let current_len = current_path.len();
            let mut children_map: crate::HashMap<String, Vec<(usize, InstanceHierarchy)>> =
                crate::HashMap::default();

            for path in self.program.frontend.instance_ids.keys() {
                if path.0.len() == current_len + 1 && path.0.starts_with(current_path) {
                    let (child_name_id, child_index) = path.0[current_len];
                    let child_name = veryl_parser::resource_table::get_str_value(child_name_id)
                        .unwrap()
                        .to_string();
                    let child_hierarchy = self.build_hierarchy(&path.0);
                    children_map
                        .entry(child_name)
                        .or_default()
                        .push((child_index, child_hierarchy));
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

        /// Build a simulator directly from SystemVerilog sources.
        #[cfg(feature = "systemverilog")]
        pub fn from_sv_sources<'a>(
            sources: Vec<(&'a str, &'a std::path::Path)>,
            top: &'a str,
        ) -> SimulatorBuilder<'a, Simulator> {
            SimulatorBuilder::<Simulator>::from_sv_sources(sources, top)
        }

        /// Build a simulator from a Veryl hierarchy with SystemVerilog children.
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

    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", feature = "experimental-arm64-backend")
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
}

#[cfg(feature = "host-runtime")]
pub use host::*;
