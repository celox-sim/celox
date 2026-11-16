//! Runtime events of a simulation: their public form, rendering, and collection from the backend buffer.

use super::*;

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
    pub(super) timed: bool,
    pub(super) buffer: Arc<RuntimeEventBuffer>,
    pub(super) layout: MemoryLayout,
    pub(super) sites: Vec<RuntimeEventSite>,
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the drain handle may run on another thread and locks the queue the simulation thread stashes events into"
    )]
    pub(super) pending_host_events: Arc<std::sync::Mutex<PendingRuntimeEvents>>,
    pub(super) read_seq: u64,
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the read sequence the drain handle advances on its thread and the simulation thread reads"
    )]
    pub(super) shared_read_seq: Arc<AtomicU64>,
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the drain handle clears it on its thread when dropped and the simulation thread checks it"
    )]
    pub(super) active: Arc<AtomicBool>,
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the drain handle sets it on its thread and the simulation thread reads it before each step"
    )]
    pub(super) backpressure: Arc<AtomicBool>,
    /// Whether a site records heap handles (`sites_record_handles`).
    pub(super) handles: bool,
    pub(super) pacing: Arc<DrainPacing>,
}

/// The threads of a simulation and of the drain that paces it, which
/// wake each other instead of spinning while one waits for the other.
#[derive(Default)]
pub(crate) struct DrainPacing {
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the simulation thread registers itself here and the drain thread unparks it"
    )]
    pub(super) simulation: std::sync::Mutex<Option<std::thread::Thread>>,
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the drain thread registers itself here and the simulation thread unparks it"
    )]
    pub(super) drain: std::sync::Mutex<Option<std::thread::Thread>>,
}

impl DrainPacing {
    /// Make the calling thread the one `slot` wakes.
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the simulation and drain threads register themselves in each other's wake slot"
    )]
    pub(super) fn register(slot: &std::sync::Mutex<Option<std::thread::Thread>>) {
        let current = std::thread::current();
        let mut thread = slot.lock().unwrap();
        if thread
            .as_ref()
            .is_none_or(|thread| thread.id() != current.id())
        {
            *thread = Some(current);
        }
    }

    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the simulation and drain threads unpark each other through these wake slots"
    )]
    pub(super) fn wake(slot: &std::sync::Mutex<Option<std::thread::Thread>>) {
        if let Some(thread) = slot.lock().unwrap().as_ref() {
            thread.unpark();
        }
    }
}

/// Whether a site of `sites` records heap handles, which only the
/// simulation's thread can resolve.
pub(super) fn sites_record_handles(sites: &[RuntimeEventSite]) -> bool {
    sites
        .iter()
        .any(|site| site.arg_is_handle.iter().any(|&handle| handle))
}

impl RuntimeEventDrain {
    /// While `enabled`, the simulation waits before each step until
    /// this drain has taken every event, so that none of a step's, up to
    /// the ring buffer's capacity, is lost while another thread keeps
    /// draining. A drain
    /// that stops draining then stalls the simulation until it drains
    /// again or is dropped. When the design records heap handles (such
    /// as `string` arguments), the simulation's thread moves the records
    /// out of the ring buffer before each step, resolving the handles,
    /// and the drain takes only those.
    pub fn set_backpressure(&self, enabled: bool) {
        self.backpressure.store(enabled, Ordering::Release);
        // A simulation waiting for room goes on when no longer paced.
        if !enabled {
            DrainPacing::wake(&self.pacing.simulation);
        }
    }

    pub fn drain(&mut self) -> Vec<RuntimeEvent> {
        self.drain_with_context(RuntimeFormatContext::default())
    }

    /// Block the calling thread until the simulation waits for this
    /// drain to take events (see [`Self::set_backpressure`]), or until
    /// `timeout` passes or the thread is unparked. Returns at once when
    /// events are waiting to be taken.
    pub fn wait(&self, timeout: std::time::Duration) {
        DrainPacing::register(&self.pacing.drain);
        // Check after registering: a simulation waiting from then on
        // wakes this thread.
        if !self.pending_host_events.lock().unwrap().events.is_empty()
                // Records with handles wait in the ring for the simulation.
                || (!self.sites.is_empty()
                    && !(self.handles && self.backpressure.load(Ordering::Acquire))
                    && self.buffer.load_atomic_u64(0, Ordering::Acquire)
                        > self.shared_read_seq.load(Ordering::Acquire))
        {
            return;
        }
        std::thread::park_timeout(timeout);
    }

    pub fn drain_with_context(&mut self, ctx: RuntimeFormatContext<'_>) -> Vec<RuntimeEvent> {
        let mut pending = self.pending_host_events.lock().unwrap();
        // Handles left in the ring are the simulation's to resolve.
        if !(self.handles && self.backpressure.load(Ordering::Acquire)) {
            self.read_seq = self.shared_read_seq.load(Ordering::Acquire);
            let events = drain_raw_runtime_events_from_buffer(
                &self.buffer,
                &self.layout,
                &self.sites,
                &mut self.read_seq,
            );
            self.shared_read_seq.store(self.read_seq, Ordering::Release);
            pending.extend(events, self.layout.runtime_event_capacity);
        }
        let events = pending.drain();
        drop(pending);
        // A simulation waiting for room goes on.
        DrainPacing::wake(&self.pacing.simulation);
        events
            .into_iter()
            .filter_map(|raw| render_raw_runtime_event(raw, &self.sites, ctx))
            .map(|event| untimed_report(event, self.timed, ctx))
            .collect()
    }
}

impl Drop for RuntimeEventDrain {
    fn drop(&mut self) {
        self.backpressure.store(false, Ordering::Release);
        self.active.store(false, Ordering::Release);
        DrainPacing::wake(&self.pacing.simulation);
    }
}

#[derive(Clone)]
pub(super) struct RuntimeEventArgValue {
    pub(super) values: Vec<u64>,
    pub(super) masks: Vec<u64>,
    pub(super) width: usize,
    pub(super) signed: bool,
    pub(super) is_string: bool,
    pub(super) float_precision: Option<celox_design::FloatPrecision>,
    /// A string handle not yet replaced by its contents.
    pub(super) handle: bool,
}

impl RuntimeEventArgValue {
    /// Replace a string handle by the bytes of its string, which the
    /// formatter prints as text.
    pub(super) fn resolve_handle(&mut self, heap: &celox_heap::Heap) {
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
    pub(super) fn resolve_handles(&mut self, heap: &celox_heap::Heap) {
        let (Self::Host { args, .. } | Self::Event { args, .. }) = self else {
            return;
        };
        for arg in args {
            arg.resolve_handle(heap);
        }
    }
}

#[derive(Clone)]
pub(super) enum RawRuntimeEvent {
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

pub(super) fn runtime_event_words_to_biguint(words: &[u64], width: usize) -> BigUint {
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

pub(super) fn runtime_event_format_arg(
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

pub(super) fn render_runtime_event_message(
    site: &RuntimeEventSite,
    args: &[RuntimeEventArgValue],
    ctx: RuntimeFormatContext<'_>,
) -> Vec<u8> {
    let time_format = site_time_format(site, args);
    render_formatted_message(site, formatted_args(site, args), ctx, &time_format)
}

/// The message of `site` for `args`, the arguments it formats, with `%t`
/// in `time_format`.
pub(super) fn render_formatted_message(
    site: &RuntimeEventSite,
    args: &[RuntimeEventArgValue],
    ctx: RuntimeFormatContext<'_>,
    time_format: &TimeFormat,
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
                out.extend_from_slice(&format_time(arg, time_format, field));
                arg_idx += 1;
            }
            // The analyzer rewrites each `%p` of a literal format; one of a
            // format read at run time prints a string in quotes and any
            // other value as `%0d` does (IEEE 1800-2023 21.2.1.6).
            'p' | 'P' if site.sizing == DisplaySizing::Ieee => {
                let Some(arg) = args.get(arg_idx) else {
                    arg_idx += 1;
                    continue;
                };
                if arg.is_string {
                    out.push(b'"');
                    out.extend_from_slice(&runtime_event_format_arg(
                        arg,
                        's',
                        site.sizing,
                        FieldSpec::default(),
                    ));
                    out.push(b'"');
                } else {
                    let field = FieldSpec {
                        width: Some(0),
                        ..FieldSpec::default()
                    };
                    out.extend_from_slice(&runtime_event_format_arg(arg, 'd', site.sizing, field));
                }
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

pub(super) fn render_raw_runtime_event(
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

/// Without a scheduler the simulation has no time: a report has the
/// time the host gives, not the one its site read.
pub(super) fn untimed_report(
    event: RuntimeEvent,
    timed: bool,
    ctx: RuntimeFormatContext<'_>,
) -> RuntimeEvent {
    match event {
        RuntimeEvent::Report(report) if !timed => RuntimeEvent::Report(Report {
            time: ctx.tb_time,
            ..report
        }),
        event => event,
    }
}

/// The arguments of an event that its message formats: all but the
/// time of a timestamped site.
pub(super) fn formatted_args<'a>(
    site: &RuntimeEventSite,
    args: &'a [RuntimeEventArgValue],
) -> &'a [RuntimeEventArgValue] {
    let args = match (site.timestamp_arg, args.split_last()) {
        (true, Some((_, args))) => args,
        _ => args,
    };
    match (site.time_format_arg, args.split_last()) {
        (true, Some((_, args))) => args,
        _ => args,
    }
}

/// The format of `%t` of an event of `site` with `args`: the one its
/// format argument captured, or the default.
pub(super) fn site_time_format(
    site: &RuntimeEventSite,
    args: &[RuntimeEventArgValue],
) -> TimeFormat {
    let args = match (site.timestamp_arg, args.split_last()) {
        (true, Some((_, args))) => args,
        _ => args,
    };
    match (site.time_format_arg, args.last()) {
        (true, Some(format)) => TimeFormat::of(format),
        _ => TimeFormat::default(),
    }
}

/// The format in which `%t` reports a time (IEEE 1800-2023 20.4.3), as a
/// `$timeformat` call sets it in the design's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TimeFormat {
    /// The powers of ten from the simulation's time unit to the reported
    /// unit.
    pub(super) units: i32,
    /// The digits after the decimal point.
    pub(super) precision: u32,
    pub(super) suffix: Vec<u8>,
    /// The fewest characters a conversion prints.
    pub(super) width: usize,
}

impl Default for TimeFormat {
    /// The format before a `$timeformat` call: the simulation's time unit,
    /// no decimal places or suffix, and 20 characters (Table 20-3).
    fn default() -> Self {
        Self {
            units: 0,
            precision: 0,
            suffix: Vec::new(),
            width: 20,
        }
    }
}

impl TimeFormat {
    /// The format that the value `arg` of the design's format variable
    /// holds (`celox_design::time_format`).
    pub(super) fn of(arg: &RuntimeEventArgValue) -> Self {
        use celox_design::time_format::{
            FIELD_WIDTH, PRECISION, SET, SUFFIX, SUFFIX_BYTES, SUFFIX_LENGTH, UNITS,
        };
        let bits = runtime_event_words_to_biguint(&arg.values, arg.width);
        let field = |offset: usize, width: usize| -> BigUint {
            (&bits >> offset) & ((BigUint::from(1u8) << width) - 1u8)
        };
        if !bits.bit(SET as u64) {
            return Self::default();
        }
        let word = |offset: usize| field(offset, 32).iter_u32_digits().next().unwrap_or(0) as i32;
        // `$timeformat` stores only a units number from 2 to -15 (IEEE
        // 1800-2023 20.4.3) counted from a time unit in the same range, and
        // a precision and field width from 0 to their most Celox prints.
        let count = |offset: usize| {
            u32::try_from(word(offset)).expect("`$timeformat` stores a checked count")
        };
        Self {
            units: word(UNITS),
            precision: count(PRECISION),
            suffix: suffix_bytes(
                &field(SUFFIX, 8 * SUFFIX_BYTES),
                (field(SUFFIX_LENGTH, 8)
                    .iter_u32_digits()
                    .next()
                    .unwrap_or(0) as usize)
                    .min(SUFFIX_BYTES),
            ),
            width: count(FIELD_WIDTH) as usize,
        }
    }
}

/// The last `length` bytes of `bits`, the bytes of a suffix with its last
/// byte lowest, NUL bytes included.
fn suffix_bytes(bits: &BigUint, length: usize) -> Vec<u8> {
    let mut bytes = bits.to_bytes_le();
    bytes.resize(length, 0);
    bytes.reverse();
    bytes
}

/// What `%t` prints for `arg`, a time in simulation time units, in
/// `format` (IEEE 1800-2023 20.4.3): the time in its units with its decimal
/// places, rounded half away from zero, also a real time, and its suffix,
/// padded to its minimum field width, which the width of the conversion
/// replaces, with spaces on the left, zeros after the sign for a leading
/// `0`, or spaces on the right for `-`. A value with unknown bits prints as
/// `%d` prints it.
fn format_time(arg: &RuntimeEventArgValue, format: &TimeFormat, field: FieldSpec) -> Vec<u8> {
    let width = field.width.unwrap_or(format.width);
    let mut text = match arg.float_precision {
        Some(kind) => {
            let bits = runtime_event_words_to_biguint(&arg.values, arg.width);
            let value = match kind {
                celox_design::FloatPrecision::F64 => {
                    f64::from_bits(bits.iter_u64_digits().next().unwrap_or(0))
                }
                celox_design::FloatPrecision::F32 => {
                    f64::from(f32::from_bits(bits.iter_u32_digits().next().unwrap_or(0)))
                }
            };
            // A finite real is an exact binary fraction, so its digits come
            // from that rational, rounded once.
            match real_ratio(value) {
                Some((numerator, binary_exponent)) => {
                    rounded_time(numerator, binary_exponent, format.units, format.precision)
                }
                None => value.to_string(),
            }
        }
        None if arg.masks.iter().any(|&mask| mask != 0) => {
            String::from_utf8_lossy(&runtime_event_format_arg(
                arg,
                'd',
                DisplaySizing::Ieee,
                FieldSpec {
                    width: Some(0),
                    ..FieldSpec::default()
                },
            ))
            .into_owned()
        }
        None => {
            let unsigned = runtime_event_words_to_biguint(&arg.values, arg.width);
            let value = if arg.signed && arg.width > 0 && unsigned.bit(arg.width as u64 - 1) {
                num_bigint::BigInt::from(unsigned) - (num_bigint::BigInt::from(1u8) << arg.width)
            } else {
                num_bigint::BigInt::from(unsigned)
            };
            decimal_time(value, format.units, format.precision)
        }
    }
    .into_bytes();
    text.extend_from_slice(&format.suffix);
    let pad = width.saturating_sub(text.len());
    let mut out = Vec::with_capacity(text.len() + pad);
    if field.left_justify {
        out.extend_from_slice(&text);
        out.resize(text.len() + pad, b' ');
    } else if field.zero_pad {
        // Zeros go after the sign, as `%d` pads with them.
        let digits = text.strip_prefix(b"-");
        if digits.is_some() {
            out.push(b'-');
        }
        out.resize(out.len() + pad, b'0');
        out.extend_from_slice(digits.unwrap_or(&text));
    } else {
        out.resize(pad, b' ');
        out.extend_from_slice(&text);
    }
    out
}

/// An integral time of `ticks` simulation time units in its units and
/// decimal places, rounded half away from zero.
fn decimal_time(ticks: num_bigint::BigInt, units: i32, precision: u32) -> String {
    rounded_time(ticks, 0, units, precision)
}

/// The finite real `value` as `numerator * 2^exponent`, exactly.
fn real_ratio(value: f64) -> Option<(num_bigint::BigInt, i32)> {
    if !value.is_finite() {
        return None;
    }
    let bits = value.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (mantissa, exponent) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), biased - 1075)
    };
    let numerator = num_bigint::BigInt::from(mantissa);
    Some((if value < 0.0 { -numerator } else { numerator }, exponent))
}

/// A time of `numerator * 2^binary_exponent` simulation time units in its
/// units, `10^units` time units, with `precision` decimal places, rounded
/// half away from zero.
fn rounded_time(
    numerator: num_bigint::BigInt,
    binary_exponent: i32,
    units: i32,
    precision: u32,
) -> String {
    use num_bigint::{BigInt, Sign};
    let ten = |power: u32| BigInt::from(10u8).pow(power);
    let two = |power: u32| BigInt::from(1u8) << power;
    // fixed = numerator * 2^binary_exponent * 10^(precision - units)
    let mut scaled = numerator * ten(precision);
    let mut divisor = BigInt::from(1u8);
    if binary_exponent >= 0 {
        scaled *= two(binary_exponent.unsigned_abs());
    } else {
        divisor *= two(binary_exponent.unsigned_abs());
    }
    if units >= 0 {
        divisor *= ten(units.unsigned_abs());
    } else {
        scaled *= ten(units.unsigned_abs());
    }
    let (quotient, remainder) = (&scaled / &divisor, &scaled % &divisor);
    let fixed = if remainder.magnitude() * 2u8 >= *divisor.magnitude() {
        match scaled.sign() {
            Sign::Minus => quotient - 1,
            _ => quotient + 1,
        }
    } else {
        quotient
    };
    fixed_point(fixed, precision)
}

/// `fixed`, an integer of the smallest of `precision` decimal places, with
/// its decimal point: `fixed / 10^precision`.
fn fixed_point(fixed: num_bigint::BigInt, precision: u32) -> String {
    let negative = fixed.sign() == num_bigint::Sign::Minus;
    let digits = fixed.magnitude().to_string();
    let precision = precision as usize;
    let digits = format!("{digits:0>width$}", width = precision + 1);
    let (whole, fraction) = digits.split_at(digits.len() - precision);
    let sign = if negative { "-" } else { "" };
    if precision == 0 {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

pub(super) fn render_runtime_event(
    site: &RuntimeEventSite,
    args: &[RuntimeEventArgValue],
    ctx: RuntimeFormatContext<'_>,
) -> RuntimeEvent {
    // The time the site issued the event, unless the host gives one.
    let issued = site
        .timestamp_arg
        .then(|| args.last().and_then(|time| time.values.first().copied()))
        .flatten();
    let ctx = RuntimeFormatContext {
        tb_time: ctx.tb_time.or(issued),
        ..ctx
    };
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

pub(super) fn collect_runtime_events(
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
        let arg_count = read_payload_u64(slot_base + RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET) as usize;
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

pub(super) fn drain_raw_runtime_events_from_buffer(
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

#[cfg(test)]
mod time_format_tests {
    use super::*;

    /// A time in larger units rounds half away from zero to its decimal
    /// places; in smaller units it gains zeros (IEEE 1800-2023 20.4.3).
    #[test]
    fn decimal_times_round_half_away_from_zero() {
        let time = |ticks: i64, units, precision| {
            decimal_time(num_bigint::BigInt::from(ticks), units, precision)
        };
        assert_eq!(time(1500, 3, 0), "2");
        assert_eq!(time(1499, 3, 0), "1");
        assert_eq!(time(-1500, 3, 0), "-2");
        assert_eq!(time(1500, 3, 3), "1.500");
        assert_eq!(time(12346, 6, 4), "0.0123");
        assert_eq!(time(-5, 3, 2), "-0.01");
        assert_eq!(time(7, -3, 1), "7000.0");
        assert_eq!(time(0, 6, 5), "0.00000");
    }

    /// A real time is scaled to its units and decimal places once, and
    /// rounded half away from zero (IEEE 1800-2023 20.4.3).
    #[test]
    fn real_times_scale_once_to_their_decimal_places() {
        let real = |ticks: f64, units, precision| {
            let arg = RuntimeEventArgValue {
                values: vec![ticks.to_bits()],
                masks: vec![0],
                width: 64,
                signed: false,
                is_string: false,
                float_precision: Some(celox_design::FloatPrecision::F64),
                handle: false,
            };
            let format = TimeFormat {
                units,
                precision,
                suffix: b" ns".to_vec(),
                width: 0,
            };
            String::from_utf8(format_time(&arg, &format, FieldSpec::default())).unwrap()
        };
        assert_eq!(real(1500.0, 3, 1), "1.5 ns");
        assert_eq!(real(1500.0, 3, 2), "1.50 ns");
        assert_eq!(real(1500.0, 3, 3), "1.500 ns");
        assert_eq!(real(1234.5, 3, 3), "1.235 ns");
        assert_eq!(real(2500.0, 3, 0), "3 ns");
        assert_eq!(real(-2500.0, 3, 0), "-3 ns");
        // The digits of the binary fraction nearest 0.1, also beyond the
        // digits of an f64, and more decimal places than 64.
        assert_eq!(real(0.1, 0, 20), "0.10000000000000000555 ns");
        assert_eq!(real(1.5, 0, 70), format!("1.5{} ns", "0".repeat(69)));
        assert_eq!(real(f64::INFINITY, 0, 3), "inf ns");
    }

    /// An integral time keeps any number of decimal places.
    #[test]
    fn decimal_times_keep_every_decimal_place() {
        let time = decimal_time(num_bigint::BigInt::from(7), 0, 100);
        assert_eq!(time, format!("7.{}", "0".repeat(100)));
    }
}
