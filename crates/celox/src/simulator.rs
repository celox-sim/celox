#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(target_arch = "x86_64")]
use crate::backend::native::{NativeBackend, SharedNativeCode};
#[cfg(not(target_arch = "wasm32"))]
use crate::{
    backend::{JitBackend, MemoryLayout, SharedJitCode, SimBackend},
    ir::{InstancePath, Program, RuntimeEventKind, RuntimeEventSite, SignalRef, VariableInfo},
    IOContext, RuntimeErrorCode,
};
use num_bigint::BigUint;

mod builder;
mod error;

pub use builder::compile_to_sir;
#[cfg(not(target_arch = "wasm32"))]
pub use builder::{DeadStorePolicy, SimulatorBuilder, SimulatorOptions};
pub use error::render_diagnostic;
pub use error::{SimulatorError, SimulatorErrorKind};

/// Hierarchical instance tree with resolved signals.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct InstanceHierarchy {
    pub module_name: String,
    pub signals: Vec<NamedSignal>,
    pub children: Vec<(String, Vec<InstanceHierarchy>)>,
}

/// A named signal with its resolved memory reference and metadata.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct NamedSignal {
    pub name: String,
    pub signal: SignalRef,
    pub info: VariableInfo,
    /// For reset signals, the name of the associated clock (from FfDeclaration).
    pub associated_clock: Option<String>,
}

/// A named event with its resolved ID and event reference.
#[cfg(not(target_arch = "wasm32"))]
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
/// uses the native x86-64 backend on x86-64 and Cranelift elsewhere.
#[cfg(not(target_arch = "wasm32"))]
pub struct Simulator<B: SimBackend = crate::DefaultBackend> {
    pub(crate) backend: B,
    pub(crate) program: Program,
    pub(crate) vcd_writer: Option<crate::vcd::VcdWriter>,
    pub(crate) dirty: bool,
    pub(crate) warnings: Vec<veryl_analyzer::AnalyzerError>,
    runtime_event_read_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    Display { message: String },
    AssertContinue { message: String },
    AssertFatal { message: String },
    Missed { count: u64 },
}

#[cfg(not(target_arch = "wasm32"))]
impl<B: SimBackend> std::fmt::Debug for Simulator<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Simulator").finish()
    }
}

struct RuntimeEventArgValue {
    values: Vec<u64>,
    masks: Vec<u64>,
    width: usize,
    signed: bool,
}

fn runtime_event_bit(words: &[u64], bit: usize) -> bool {
    words
        .get(bit / 64)
        .map(|word| ((word >> (bit % 64)) & 1) != 0)
        .unwrap_or(false)
}

fn runtime_event_has_mask(arg: &RuntimeEventArgValue) -> bool {
    for bit in 0..arg.width {
        if runtime_event_bit(&arg.masks, bit) {
            return true;
        }
    }
    false
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

fn format_runtime_arg_decimal(arg: &RuntimeEventArgValue) -> String {
    if runtime_event_has_mask(arg) {
        return "x".to_string();
    }
    let value = runtime_event_words_to_biguint(&arg.values, arg.width);
    if arg.signed && arg.width > 0 && runtime_event_bit(&arg.values, arg.width - 1) {
        let modulus = BigUint::from(1u8) << arg.width;
        format!("-{}", modulus - value)
    } else {
        value.to_string()
    }
}

fn format_runtime_arg_binary(arg: &RuntimeEventArgValue) -> String {
    if !runtime_event_has_mask(arg) {
        return runtime_event_words_to_biguint(&arg.values, arg.width).to_str_radix(2);
    }
    let digits = arg.width.max(1);
    let mut out = String::with_capacity(digits);
    for bit in (0..digits).rev() {
        if runtime_event_bit(&arg.masks, bit) {
            out.push('x');
        } else if runtime_event_bit(&arg.values, bit) {
            out.push('1');
        } else {
            out.push('0');
        }
    }
    out
}

fn format_runtime_arg_radix_with_mask(arg: &RuntimeEventArgValue, bits_per_digit: usize) -> String {
    let digits = arg.width.div_ceil(bits_per_digit).max(1);
    let mut out = String::with_capacity(digits);
    for digit_idx in (0..digits).rev() {
        let start = digit_idx * bits_per_digit;
        let end = (start + bits_per_digit).min(arg.width);
        if (start..end).any(|bit| runtime_event_bit(&arg.masks, bit)) {
            out.push('x');
            continue;
        }
        let mut digit = 0u32;
        for bit in start..end {
            if runtime_event_bit(&arg.values, bit) {
                digit |= 1 << (bit - start);
            }
        }
        out.push(char::from_digit(digit, 1 << bits_per_digit).unwrap());
    }
    out
}

fn format_runtime_arg_hex(arg: &RuntimeEventArgValue, upper: bool) -> String {
    let mut out = if runtime_event_has_mask(arg) {
        format_runtime_arg_radix_with_mask(arg, 4)
    } else {
        runtime_event_words_to_biguint(&arg.values, arg.width).to_str_radix(16)
    };
    if upper {
        out.make_ascii_uppercase();
    }
    out
}

fn format_runtime_arg_octal(arg: &RuntimeEventArgValue) -> String {
    if runtime_event_has_mask(arg) {
        format_runtime_arg_radix_with_mask(arg, 3)
    } else {
        runtime_event_words_to_biguint(&arg.values, arg.width).to_str_radix(8)
    }
}

fn format_runtime_arg_char(arg: &RuntimeEventArgValue) -> String {
    if runtime_event_has_mask(arg) {
        "x".to_string()
    } else {
        let value = arg.values.first().copied().unwrap_or(0);
        let value = if arg.width >= 64 {
            value
        } else if arg.width == 0 {
            0
        } else {
            value & ((1u64 << arg.width) - 1)
        };
        char::from_u32(value as u32)
            .unwrap_or('\u{fffd}')
            .to_string()
    }
}

fn render_runtime_event_message(site: &RuntimeEventSite, args: &[RuntimeEventArgValue]) -> String {
    let Some(template) = site.template.as_deref() else {
        return args
            .iter()
            .map(format_runtime_arg_decimal)
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
        let fallback;
        let arg = if let Some(arg) = args.get(arg_idx) {
            arg
        } else {
            fallback = RuntimeEventArgValue {
                values: vec![0],
                masks: vec![0],
                width: 64,
                signed: false,
            };
            &fallback
        };
        arg_idx += 1;
        match spec {
            'x' => out.push_str(&format_runtime_arg_hex(arg, false)),
            'X' => out.push_str(&format_runtime_arg_hex(arg, true)),
            'b' | 'B' => out.push_str(&format_runtime_arg_binary(arg)),
            'o' | 'O' => out.push_str(&format_runtime_arg_octal(arg)),
            'c' => out.push_str(&format_runtime_arg_char(arg)),
            _ => out.push_str(&format_runtime_arg_decimal(arg)),
        }
    }
    out
}

// ── Generic methods available for any backend ────────────────────────
#[cfg(not(target_arch = "wasm32"))]
impl<B: SimBackend> Simulator<B> {
    fn decorate_runtime_error(&self, err: RuntimeErrorCode) -> RuntimeErrorCode {
        match err {
            RuntimeErrorCode::DetectedTrueLoopCode(code) => {
                let Some(info) = self.program.runtime_errors.get(&code) else {
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
        program: Program,
        warnings: Vec<veryl_analyzer::AnalyzerError>,
    ) -> Self {
        Self {
            backend,
            program,
            vcd_writer: None,
            dirty: false,
            warnings,
            runtime_event_read_seq: 0,
        }
    }

    pub fn drain_runtime_events(&mut self) -> Vec<RuntimeEvent> {
        use crate::backend::memory_layout::{
            RUNTIME_EVENT_CAPACITY, RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
            RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET, RUNTIME_EVENT_SLOT_SEQ_OFFSET,
            RUNTIME_EVENT_SLOT_SITE_OFFSET, RUNTIME_EVENT_WRITING,
        };

        let layout = self.backend.layout();
        let (ptr, size) = self.backend.memory_as_ptr();
        let base = layout.runtime_event_base_offset;
        if base + RUNTIME_EVENT_HEADER_SIZE > size {
            return Vec::new();
        }
        let read_u64 = |offset: usize| -> u64 {
            unsafe { std::ptr::read_volatile(ptr.add(offset) as *const u64) }
        };
        let write_seq = read_u64(base);
        let mut events = Vec::new();
        let capacity = RUNTIME_EVENT_CAPACITY as u64;
        if self.runtime_event_read_seq + capacity < write_seq {
            let new_read = write_seq - capacity;
            events.push(RuntimeEvent::Missed {
                count: new_read - self.runtime_event_read_seq,
            });
            self.runtime_event_read_seq = new_read;
        }
        while self.runtime_event_read_seq < write_seq {
            let seq = self.runtime_event_read_seq;
            let slot = (seq as usize) & (layout.runtime_event_capacity - 1);
            let slot_base =
                base + RUNTIME_EVENT_HEADER_SIZE + slot * layout.runtime_event_slot_size;
            let published = read_u64(slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET);
            if published == RUNTIME_EVENT_WRITING || published != seq {
                break;
            }
            let site_id = read_u64(slot_base + RUNTIME_EVENT_SLOT_SITE_OFFSET) as usize;
            if let Some(site) = self.program.runtime_event_sites.get(site_id) {
                let site_layout = layout.runtime_event_site_layouts.get(site_id);
                let arg_count = read_u64(slot_base + RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET) as usize;
                let arg_count = arg_count.min(site.arg_widths.len());
                let mut args = Vec::with_capacity(arg_count);
                if let Some(site_layout) = site_layout {
                    for idx in 0..arg_count {
                        let Some(arg_layout) = site_layout.args.get(idx) else {
                            break;
                        };
                        let mut values = Vec::with_capacity(arg_layout.word_count);
                        let mut masks = Vec::with_capacity(arg_layout.word_count);
                        for word_idx in 0..arg_layout.word_count {
                            values.push(read_u64(
                                slot_base
                                    + RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                                    + (arg_layout.value_word_offset + word_idx) * 8,
                            ));
                            masks.push(read_u64(
                                slot_base
                                    + RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                                    + (arg_layout.mask_word_offset + word_idx) * 8,
                            ));
                        }
                        args.push(RuntimeEventArgValue {
                            values,
                            masks,
                            width: site.arg_widths.get(idx).copied().unwrap_or(64),
                            signed: site.arg_signed.get(idx).copied().unwrap_or(false),
                        });
                    }
                }
                let message = render_runtime_event_message(site, &args);
                events.push(match site.kind {
                    RuntimeEventKind::Display => RuntimeEvent::Display { message },
                    RuntimeEventKind::AssertContinue => RuntimeEvent::AssertContinue { message },
                    RuntimeEventKind::AssertFatal => RuntimeEvent::AssertFatal { message },
                });
            }
            self.runtime_event_read_seq += 1;
        }
        events
    }

    /// Returns a reference to the compiled SIR program.
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// Returns a reference to the backend (for signal/event resolution).
    pub fn backend_ref(&self) -> &B {
        &self.backend
    }

    /// Returns analyzer warnings emitted during compilation.
    pub fn warnings(&self) -> &[veryl_analyzer::AnalyzerError] {
        &self.warnings
    }

    /// Captures the current state of all signals and writes them to the VCD file.
    pub fn dump(&mut self, timestamp: u64) {
        if self.dirty {
            self.backend.eval_comb().unwrap();
            self.dirty = false;
        }
        if let Some(ref mut writer) = self.vcd_writer {
            let (ptr, size) = self.backend.memory_as_ptr();
            let memory = unsafe { std::slice::from_raw_parts(ptr, size) };
            writer.dump(timestamp, memory).unwrap();
        }
    }

    /// Sets a signal value and marks combinational logic as dirty.
    pub fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
        self.backend.set(signal, val);
        self.dirty = true;
    }

    /// Sets a wide signal value and marks combinational logic as dirty.
    pub fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
        self.backend.set_wide(signal, val);
        self.dirty = true;
    }

    /// Sets a four-state signal value and marks combinational logic as dirty.
    pub fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint) {
        self.backend.set_four_state(signal, val, mask);
        self.dirty = true;
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
        Ok(())
    }

    pub(crate) fn eval_comb_checked(&mut self) -> Result<(), RuntimeErrorCode> {
        self.backend
            .eval_comb()
            .map_err(|e| self.decorate_runtime_error(e))
    }

    pub(crate) fn eval_apply_ff_at_checked(
        &mut self,
        event: B::Event,
    ) -> Result<(), RuntimeErrorCode> {
        self.backend
            .eval_apply_ff_at(event)
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

    pub(crate) fn apply_ff_at_checked(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        self.backend
            .apply_ff_at(event)
            .map_err(|e| self.decorate_runtime_error(e))
    }

    /// Manually triggers a clock or event to process sequential logic.
    pub fn tick(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        if self.dirty {
            self.eval_comb_checked()?;
            self.eval_apply_ff_at_checked(event)?;
            self.dirty = false;
        } else {
            self.eval_apply_ff_at_checked(event)?;
        }
        self.eval_comb_checked()?;
        self.dirty = false;
        Ok(())
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
            self.backend.eval_comb().unwrap();
            self.dirty = false;
        }
        self.backend.get_as(signal)
    }

    /// Retrieves the current value of a variable using a pre-resolved [`SignalRef`] handle.
    /// Lazily evaluates combinational logic if the state is dirty.
    pub fn get(&mut self, signal: SignalRef) -> BigUint {
        if self.dirty {
            self.backend.eval_comb().unwrap();
            self.dirty = false;
        }
        self.backend.get(signal)
    }

    /// Retrieves the current 4-state value (value, mask) of a variable using a [`SignalRef`] handle.
    /// Lazily evaluates combinational logic if the state is dirty.
    pub fn get_four_state(&mut self, signal: SignalRef) -> (BigUint, BigUint) {
        if self.dirty {
            self.backend.eval_comb().unwrap();
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
    /// paths without the original [`Program`].
    pub fn build_vcd_descs(&self, four_state_mode: bool) -> Vec<crate::vcd::VcdSignalDesc> {
        let mut descs = Vec::new();
        let mut sorted_instances: Vec<_> = self.program.instance_module.iter().collect();
        sorted_instances.sort_by_key(|(id, _)| *id);

        for (instance_id, module_id) in sorted_instances {
            let variables = &self.program.module_variables[module_id];
            let path_index = &self.program.module_var_path_index[module_id];
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

                let addr = crate::ir::AbsoluteAddr {
                    instance_id: *instance_id,
                    var_id: info.id,
                };
                let signal = self.backend.resolve_signal(&addr);

                descs.push(crate::vcd::VcdSignalDesc {
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
        match self.program.instance_ids.get(&InstancePath(path_str_ids)) {
            Some(&instance_id) => self.build_signals_for_instance(instance_id),
            None => Vec::new(),
        }
    }

    /// Builds the list of named signals for a given instance.
    ///
    /// Variables with ambiguous VarPaths (multiple scoped locals sharing the
    /// same name) are excluded — they cannot be addressed by path and would
    /// cause silent overwrites in name-keyed maps such as the layout JSON.
    fn build_signals_for_instance(&self, instance_id: crate::ir::InstanceId) -> Vec<NamedSignal> {
        let module_id = &self.program.instance_module[&instance_id];
        let module_vars = &self.program.module_variables[module_id];
        let path_index = &self.program.module_var_path_index[module_id];

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
            let addr = crate::ir::AbsoluteAddr {
                instance_id,
                var_id: info.id,
            };
            let signal = self.backend.resolve_signal(&addr);

            // Resolve associated clock for reset signals
            let associated_clock = self
                .program
                .reset_clock_map
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
    pub fn tick_by_id_n(&mut self, event_id: usize, count: u32) -> Result<(), RuntimeErrorCode> {
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
            .instance_ids
            .get(&InstancePath(current_path.to_vec()))
            .expect("instance not found");
        let module_id = &self.program.instance_module[instance_id];
        let module_name = self
            .program
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

        for path in self.program.instance_ids.keys() {
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
#[cfg(not(target_arch = "wasm32"))]
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
}

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(target_arch = "x86_64")]
impl Simulator<NativeBackend> {
    /// Returns the shared compiled native code, allowing it to be reused
    /// for creating additional simulator instances without recompilation.
    pub fn shared_code(&self) -> Arc<SharedNativeCode> {
        self.backend.shared_code()
    }

    /// Create a simulator from pre-compiled shared native code.
    pub fn from_shared(shared: Arc<SharedNativeCode>, program: crate::ir::Program) -> Self {
        let backend = NativeBackend::from_shared(shared);
        Self::with_backend_and_program(backend, program, vec![])
    }

    /// Consume the simulator and return the inner native backend.
    pub fn into_backend(self) -> NativeBackend {
        self.backend
    }
}
