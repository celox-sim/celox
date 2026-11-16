mod plan;

use celox_state_layout::TRACE_GROUP_BYTES;
use num_bigint::BigUint;
use plan::TracePlan;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::mem::MaybeUninit;
use std::path::Path;

/// Describes a signal for VCD recording.
///
/// Self-contained — does not reference any IR types. Can be cached
/// alongside a shared backend artifact so that VCD
/// works even on cache-hit paths.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VcdSignalDesc {
    /// VCD scope name (e.g. instance path).
    pub scope: String,
    /// Signal name within the scope.
    pub name: String,
    /// Byte offset in JIT memory (stable region).
    pub offset: usize,
    /// Bit width.
    pub width: usize,
    /// Whether this signal has a 4-state mask region immediately after the value.
    pub is_4state: bool,
}

/// Describes a signal whose value is supplied by an external runtime rather
/// than stored in Celox's flat simulation memory.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VcdExternalSignalDesc {
    pub scope: String,
    pub name: String,
    pub width: usize,
}

struct VcdHeaderSignal {
    scope: String,
    name: String,
    width: usize,
}

/// Finalized with the header: an optional space, the ID, and a newline.
/// Most IDs fit in one fixed-size copy; retain arbitrary-length IDs as well.
enum VcdRecordSuffix {
    Inline { bytes: [u8; 8], len: u8 },
    Long(Box<[u8]>),
}

impl VcdRecordSuffix {
    fn new(width: usize, id: &str) -> Self {
        let prefix = usize::from(width != 1);
        let len = prefix + id.len() + 1;
        if len <= 8 {
            let mut bytes = [b' '; 8];
            bytes[prefix..len - 1].copy_from_slice(id.as_bytes());
            bytes[len - 1] = b'\n';
            Self::Inline {
                bytes,
                len: len as u8,
            }
        } else {
            let mut bytes = Vec::with_capacity(len);
            if prefix != 0 {
                bytes.push(b' ');
            }
            bytes.extend_from_slice(id.as_bytes());
            bytes.push(b'\n');
            Self::Long(bytes.into_boxed_slice())
        }
    }

    fn capacity(&self) -> usize {
        match self {
            Self::Inline { .. } => 8,
            Self::Long(bytes) => bytes.len(),
        }
    }

    /// Initializes the returned number of bytes, plus padding for short IDs.
    #[inline]
    fn encode(&self, out: &mut [MaybeUninit<u8>]) -> usize {
        match self {
            Self::Inline { bytes, len } => {
                copy_encoded(out, bytes);
                *len as usize
            }
            Self::Long(bytes) => {
                copy_encoded(out, bytes);
                bytes.len()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct VcdStatistics {
    pub comparisons: u64,
    pub changes: u64,
    pub value_bytes: u64,
}

pub struct VcdWriter<W: Write = File> {
    output: VcdOutput<W>,
    headers: Vec<VcdHeaderSignal>,
    plan: TracePlan,
    groups: fxhash::FxHashMap<usize, Vec<usize>>,
    selected: Vec<usize>,
    /// Consumed groups retained after a failed dump; empty after success.
    activity: Vec<usize>,
    /// Set when memory changed without marking activity (a restored state),
    /// so the next backend dump scans every signal.
    rescan_pending: bool,
    /// Signal descriptions, kept to restart the waveform in another file.
    descs: Vec<VcdSignalDesc>,
    external_descs: Vec<VcdExternalSignalDesc>,
    timestamp: u64,
    time_precision: celox_design::TimePrecision,
    header_written: bool,
    /// Complete header commands awaiting emission after a byte-limit pause.
    header_records: Option<std::collections::VecDeque<Vec<u8>>>,
    initial_values_written: bool,
    external_count: usize,
    suspended: bool,
}

struct VcdOutput<W: Write> {
    writer: BufWriter<W>,
    /// Complete records accepted by the encoder. Retained on I/O errors because
    /// the trace plan already caches their values. Headers and timestamps also
    /// use this queue, and are drained before any value records are appended.
    /// Successful dumps hand the tail to BufWriter so Drop owns pending output.
    encoded: Vec<u8>,
    /// Bytes already accepted by BufWriter, including short writes before an error.
    written: usize,
    encoded_changes: u64,
    accepted_bytes: u64,
    byte_limit: Option<u64>,
    limit_comment_written: bool,
    stats: VcdStatistics,
}

impl VcdWriter<File> {
    pub fn new<P: AsRef<Path>>(path: P, descs: &[VcdSignalDesc]) -> std::io::Result<Self> {
        Self::new_with_time_precision(path, descs, celox_design::TimePrecision::default())
    }

    /// Create a waveform whose timestamps use the design's global tick precision.
    pub fn new_with_time_precision<P: AsRef<Path>>(
        path: P,
        descs: &[VcdSignalDesc],
        time_precision: celox_design::TimePrecision,
    ) -> std::io::Result<Self> {
        Ok(Self::from_writer_with_time_precision(
            File::create(path)?,
            descs,
            time_precision,
        ))
    }

    /// Finish the current file and continue the waveform, with the same
    /// signals, in a new file at `path`. The first dump there records every
    /// value, and its timestamps start over.
    pub fn restart<P: AsRef<Path>>(&mut self, path: P) -> std::io::Result<()> {
        self.flush()?;
        let mut next = Self::new_with_time_precision(path, &self.descs, self.time_precision)?;
        next.add_external_signals(&self.external_descs)?;
        next.suspended = self.suspended;
        next.output.byte_limit = self.output.byte_limit;
        *self = next;
        Ok(())
    }
}

impl<W: Write> VcdWriter<W> {
    pub fn from_writer(writer: W, descs: &[VcdSignalDesc]) -> Self {
        Self::from_writer_with_time_precision(writer, descs, celox_design::TimePrecision::default())
    }

    /// Create a writer with an explicit physical duration for each timestamp tick.
    pub fn from_writer_with_time_precision(
        writer: W,
        descs: &[VcdSignalDesc],
        time_precision: celox_design::TimePrecision,
    ) -> Self {
        let mut plan = TracePlan::default();
        let mut groups: fxhash::FxHashMap<usize, Vec<usize>> = Default::default();
        let headers = descs
            .iter()
            .enumerate()
            .map(|(index, desc)| {
                plan.add_memory(desc);
                let group = desc.offset / TRACE_GROUP_BYTES;
                groups.entry(group).or_default().push(index);
                VcdHeaderSignal {
                    scope: desc.scope.clone(),
                    name: desc.name.clone(),
                    width: desc.width,
                }
            })
            .collect::<Vec<_>>();
        Self {
            output: VcdOutput {
                writer: BufWriter::with_capacity(256 * 1024, writer),
                encoded: Vec::new(),
                written: 0,
                encoded_changes: 0,
                accepted_bytes: 0,
                byte_limit: None,
                limit_comment_written: false,
                stats: VcdStatistics::default(),
            },
            headers,
            plan,
            groups,
            selected: Vec::new(),
            activity: Vec::new(),
            rescan_pending: false,
            descs: descs.to_vec(),
            external_descs: Vec::new(),
            timestamp: 0,
            time_precision,
            header_written: false,
            header_records: None,
            initial_values_written: false,
            external_count: 0,
            suspended: false,
        }
    }

    /// Publish buffered output, also reporting errors that Drop cannot report.
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.output.write_encoded()?;
        self.output.writer.flush()
    }

    pub fn statistics(&self) -> VcdStatistics {
        self.output.stats
    }
    pub fn get_ref(&self) -> &W {
        self.output.writer.get_ref()
    }

    /// The underlying writer. Writing to it directly bypasses the buffered
    /// output, like [`BufWriter::get_mut`].
    pub fn get_mut(&mut self) -> &mut W {
        self.output.writer.get_mut()
    }

    /// The last timestamp written to the file, if any. Later dumps must not
    /// be earlier.
    pub fn last_timestamp(&self) -> Option<u64> {
        self.header_written.then_some(self.timestamp)
    }

    /// Make the next [`Self::dump_backend`] compare every signal: memory
    /// changed without marking VCD activity, for example by restoring a
    /// checkpoint.
    pub fn rescan(&mut self) {
        self.rescan_pending = true;
    }

    /// Set the maximum output size requested by `$dumplimit`. Zero removes
    /// the limit. Complete records may take the file beyond the limit.
    pub fn set_byte_limit(&mut self, limit: u64) {
        if self.output.limit_reached() {
            // Skipped output may have consumed the backend's activity flags,
            // and a partial initial snapshot has not initialized every value.
            self.rescan_pending = true;
            self.initial_values_written = false;
        }
        self.output.byte_limit = (limit != 0).then_some(limit);
        self.output.limit_comment_written = false;
    }

    fn limit_reached(&mut self) -> bool {
        let reached = self.output.limit_reached();
        if reached {
            self.output.note_limit();
        }
        reached
    }

    /// Emit a SystemVerilog VCD checkpoint. `off` writes unknowns and
    /// suspends changes; `on` writes current values and resumes; `all`
    /// writes current values without changing suspension (21.7.1.3–4).
    pub fn checkpoint(
        &mut self,
        command: &str,
        timestamp: u64,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
    ) -> std::io::Result<()> {
        if !matches!(command, "dumpoff" | "dumpon" | "dumpall") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid VCD checkpoint",
            ));
        }
        self.validate_external_count(external.len())?;
        // Suppressing header or value records must not suppress controls.
        match command {
            "dumpoff" => self.suspended = true,
            "dumpon" => self.suspended = false,
            _ => {}
        }
        if self.limit_reached() || !self.begin_timestamp(timestamp)? {
            return Ok(());
        }
        writeln!(self.output.encoded, "${command}")?;
        if command == "dumpoff" {
            for index in 0..self.headers.len() {
                if self.output.limit_reached() {
                    break;
                }
                let width = self.headers[index].width;
                let size = width.div_ceil(8);
                self.output.encoded.reserve(width + 16);
                let len = encode_value(
                    self.output.encoded.spare_capacity_mut(),
                    width,
                    &vec![255; size],
                    &vec![255; size],
                );
                self.output.finish_record(len, self.plan.suffix(index))?;
            }
            self.suspended = true;
        } else {
            self.plan
                .dump::<true, W>(memory, external, None, &mut self.output)?;
            self.initial_values_written = true;
            if command == "dumpon" {
                self.suspended = false;
            }
        }
        writeln!(self.output.encoded, "$end")?;
        self.output.note_limit();
        self.rescan_pending = true;
        Ok(())
    }

    fn begin_timestamp(&mut self, timestamp: u64) -> std::io::Result<bool> {
        if self.header_written && timestamp < self.timestamp {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "VCD timestamp {timestamp} is earlier than the last dumped timestamp {}; start a new file to record a rewound simulation",
                    self.timestamp
                ),
            ));
        }
        self.output.write_encoded()?;
        if !self.header_written && !self.write_header()? {
            return Ok(false);
        }
        if self.limit_reached() {
            return Ok(false);
        }
        if timestamp > self.timestamp || timestamp == 0 {
            writeln!(self.output.encoded, "#{timestamp}")?;
            self.timestamp = timestamp;
            self.output.write_encoded()?;
        }
        Ok(true)
    }

    /// Dump as the backend's sole incremental waveform observer. Multiple
    /// writers must consume activity once and share it through
    /// `dump_with_activity`, since each consumption clears the pending flags.
    pub fn dump_backend<B: crate::backend::SimBackend>(
        &mut self,
        timestamp: u64,
        backend: &mut B,
        external: &[(BigUint, BigUint)],
    ) -> std::io::Result<()> {
        // Reject recoverable input errors before consuming pending writes.
        self.validate_external_count(external.len())?;
        let mut activity = std::mem::take(&mut self.activity);
        let tracked = if activity.is_empty() {
            backend.take_vcd_activity(&mut activity)
        } else {
            // Consumption clears its destination. Keep failed-dump activity
            // and merge writes made since that attempt, including duplicates.
            // Only this retry path needs a second allocation.
            let mut new_activity = Vec::new();
            let tracked = backend.take_vcd_activity(&mut new_activity);
            activity.extend(new_activity);
            activity.sort_unstable();
            activity.dedup();
            tracked
        };
        let (ptr, size) = backend.memory_as_ptr();
        // SAFETY: the backend owns the image and cannot run during this dump.
        let memory = unsafe { std::slice::from_raw_parts(ptr, size) };
        let tracked = tracked && !self.rescan_pending;
        let result =
            self.dump_with_activity(timestamp, memory, external, tracked.then_some(&activity));
        if result.is_ok() {
            activity.clear();
            self.rescan_pending = false;
        }
        self.activity = activity;
        result
    }

    pub fn into_inner(mut self) -> std::io::Result<W> {
        self.flush()?;
        self.output
            .writer
            .into_inner()
            .map_err(|error| error.into_error())
    }

    /// Adds externally supplied signals before the first dump. VCD headers
    /// cannot be extended after value changes have started.
    pub fn add_external_signals(&mut self, descs: &[VcdExternalSignalDesc]) -> std::io::Result<()> {
        if descs.is_empty() {
            return Ok(());
        }
        if self.external_count != 0 {
            let existing = self.headers[self.headers.len() - self.external_count..]
                .iter()
                .zip(descs)
                .all(|(signal, desc)| {
                    signal.scope == desc.scope
                        && signal.name == desc.name
                        && signal.width == desc.width
                });
            if existing && self.external_count == descs.len() {
                return Ok(());
            }
        }
        if self.header_written || self.header_records.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cannot add external VCD signals after the first dump",
            ));
        }
        self.external_descs.extend_from_slice(descs);
        for desc in descs {
            let index = self.external_count;
            self.external_count += 1;
            self.headers.push(VcdHeaderSignal {
                scope: desc.scope.clone(),
                name: desc.name.clone(),
                width: desc.width,
            });
            self.plan.add_external(index, desc.width);
        }
        Ok(())
    }

    #[cold]
    fn prepare_header(&mut self) -> std::collections::VecDeque<Vec<u8>> {
        let mut records = std::collections::VecDeque::new();
        records.push_back(
            format!(
                "$date\n  {}\n$end\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
            )
            .into_bytes(),
        );
        records.push_back(b"$version\n  celox\n$end\n".to_vec());
        let (magnitude, unit) = self.time_precision.vcd_timescale();
        records.push_back(format!("$timescale {magnitude}{unit} $end\n").into_bytes());
        let mut scope_order = Vec::<String>::new();
        let mut scope_groups = Vec::<Vec<usize>>::new();
        let mut scope_idx = fxhash::FxHashMap::<String, usize>::default();
        for (signal_index, signal) in self.headers.iter().enumerate() {
            if let Some(index) = scope_idx.get(&signal.scope).copied() {
                scope_groups[index].push(signal_index);
            } else {
                let index = scope_order.len();
                scope_idx.insert(signal.scope.clone(), index);
                scope_order.push(signal.scope.clone());
                scope_groups.push(vec![signal_index]);
            }
        }
        let mut next_id = 0;
        for (scope, group) in scope_order.iter().zip(scope_groups) {
            records.push_back(format!("$scope module {scope} $end\n").into_bytes());
            for signal_index in group {
                let signal = &mut self.headers[signal_index];
                let id = Self::generate_vcd_id(next_id);
                *self.plan.suffix(signal_index) = VcdRecordSuffix::new(signal.width, &id);
                next_id += 1;
                records.push_back(
                    format!("$var wire {} {} {} $end\n", signal.width, id, signal.name)
                        .into_bytes(),
                );
            }
            records.push_back(b"$upscope $end\n".to_vec());
        }
        records.push_back(b"$enddefinitions $end\n".to_vec());
        records.push_back(b"$dumpvars\n$end\n".to_vec());
        records
    }

    #[cold]
    fn write_header(&mut self) -> std::io::Result<bool> {
        if self.header_records.is_none() {
            self.header_records = Some(self.prepare_header());
        }
        while self
            .header_records
            .as_ref()
            .is_some_and(|records| !records.is_empty())
        {
            if self.limit_reached() {
                return Ok(false);
            }
            let record = self.header_records.as_mut().unwrap().pop_front().unwrap();
            self.output.encoded.extend_from_slice(&record);
            self.output.write_encoded()?;
        }
        self.header_records = None;
        let record_capacity = self
            .headers
            .iter()
            .enumerate()
            .map(|(index, signal)| {
                signal.width.max(1)
                    + usize::from(signal.width != 1)
                    + self.plan.suffix(index).capacity()
            })
            .max()
            .map(|max_record| self.output.writer.capacity() + max_record);
        if let Some(capacity) = record_capacity {
            // A block can cross the writer's capacity by one complete record.
            // Include the short suffix's padding, even for scalar-only traces.
            // Integer SIMD stores also fit within the full declared width.
            // Reserve here so record encoding never needs to grow the Vec.
            self.output
                .encoded
                .reserve(capacity.saturating_sub(self.output.encoded.len()));
        }
        self.header_written = true;
        self.output.write_encoded()?;
        if let Some(capacity) = record_capacity {
            // A large header must not permanently enlarge the value buffer.
            self.output.encoded.shrink_to(capacity);
        }
        Ok(true)
    }

    fn generate_vcd_id(num: usize) -> String {
        let mut id = String::new();
        let mut n = num;
        loop {
            let char = ((n % 94) + 33) as u8 as char;
            id.push(char);
            if n < 94 {
                break;
            }
            n = (n / 94) - 1;
        }
        id.chars().rev().collect()
    }

    /// Full-scan reference path, also suitable for uninstrumented/raw memory.
    pub fn dump(&mut self, timestamp: u64, memory: &[u8]) -> std::io::Result<()> {
        self.dump_with_external(timestamp, memory, &[])
    }

    pub fn dump_with_external(
        &mut self,
        timestamp: u64,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
    ) -> std::io::Result<()> {
        self.dump_with_activity(timestamp, memory, external, None)
    }

    fn validate_external_count(&self, count: usize) -> std::io::Result<()> {
        if count != self.external_count {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "expected {} external VCD values, got {count}",
                    self.external_count
                ),
            ));
        }
        Ok(())
    }

    /// `activity` contains unique physical group IDs consumed from TraceLayout.
    /// None requests a full scan. Initial values and external signals are always
    /// observed, including when no generated store has executed.
    pub fn dump_with_activity(
        &mut self,
        timestamp: u64,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
        activity: Option<&[usize]>,
    ) -> std::io::Result<()> {
        self.validate_external_count(external.len())?;
        if self.suspended || self.limit_reached() {
            return Ok(());
        }
        let first_dump = !self.initial_values_written;
        if !self.begin_timestamp(timestamp)? {
            return Ok(());
        }
        self.selected.clear();
        // Dense activity is cheaper to walk directly in registration order.
        let sparse = activity
            .filter(|activity| !first_dump && activity.len() < self.groups.len().div_ceil(2));
        if let Some(activity) = sparse {
            for &group in activity {
                if let Some(indices) = self.groups.get(&group) {
                    self.selected.extend_from_slice(indices);
                }
            }
            self.selected
                .extend(self.headers.len() - self.external_count..self.headers.len());
            // Preserve registration order even when homes are laid out differently.
            self.selected.sort_unstable();
            self.selected.dedup();
            if self.selected.is_empty() {
                return Ok(());
            }
        }
        if first_dump {
            self.plan
                .dump::<true, W>(memory, external, None, &mut self.output)?;
            self.initial_values_written = true;
        } else {
            self.plan.dump::<false, W>(
                memory,
                external,
                sparse.map(|_| self.selected.as_slice()),
                &mut self.output,
            )?;
        }
        self.output.note_limit();
        Ok(())
    }
}

impl<W: Write> VcdOutput<W> {
    #[inline]
    fn limit_reached(&self) -> bool {
        self.byte_limit
            .is_some_and(|limit| self.accepted_bytes + self.encoded.len() as u64 >= limit)
    }

    fn note_limit(&mut self) {
        if self.limit_reached() && !self.limit_comment_written {
            self.encoded
                .extend_from_slice(b"$comment VCD dump limit reached $end\n");
            self.limit_comment_written = true;
        }
    }

    // Keep suffix publication with the value encoder so SIMD constants and
    // output state can stay in registers between records.
    #[inline(always)]
    fn finish_record(&mut self, value_len: usize, suffix: &VcdRecordSuffix) -> std::io::Result<()> {
        let suffix_len = suffix.encode(&mut self.encoded.spare_capacity_mut()[value_len..]);
        // SAFETY: both encoders initialize their returned lengths in checked
        // spare capacity. SIMD/suffix padding stays outside the visible length.
        unsafe {
            self.encoded
                .set_len(self.encoded.len() + value_len + suffix_len);
        }
        self.encoded_changes += 1;
        if self.encoded.len() >= self.writer.capacity() {
            self.write_encoded()?;
        }
        Ok(())
    }

    #[inline]
    fn write_encoded(&mut self) -> std::io::Result<()> {
        if !self.encoded.is_empty() {
            // Full blocks bypass BufWriter's internal copy. A dump's final
            // partial block stays buffered with the timestamps and header.
            // Unlike write_all, retain progress if a later short write fails.
            let remaining = &self.encoded[self.written..];
            match self.writer.write(remaining) {
                Ok(count) if count == remaining.len() => {}
                result => self.finish_short_write(result)?,
            }
            if self.encoded_changes != 0 {
                self.stats.changes += self.encoded_changes;
                self.stats.value_bytes += self.encoded.len() as u64;
            }
            self.accepted_bytes += self.encoded.len() as u64;
            self.encoded.clear();
            self.written = 0;
            self.encoded_changes = 0;
        }
        Ok(())
    }

    #[cold]
    fn finish_short_write(&mut self, mut result: std::io::Result<usize>) -> std::io::Result<()> {
        loop {
            match result {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(count) => self.written += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
            if self.written == self.encoded.len() {
                return Ok(());
            }
            result = self.writer.write(&self.encoded[self.written..]);
        }
    }
}

fn last_mask(width: usize) -> u8 {
    if width.is_multiple_of(8) {
        0xff
    } else {
        ((1u16 << (width % 8)) - 1) as u8
    }
}

pub(crate) fn plane_equal(old: &[u8], value: &[u8], width: usize) -> bool {
    let Some((&last, prefix)) = old.split_last() else {
        return true;
    };
    if value.len() >= old.len() {
        prefix == &value[..prefix.len()] && last == value[prefix.len()] & last_mask(width)
    } else {
        old.iter().enumerate().all(|(i, &byte)| {
            byte == value.get(i).copied().unwrap_or(0)
                & if i + 1 == old.len() {
                    last_mask(width)
                } else {
                    0xff
                }
        })
    }
}

pub(crate) fn copy_plane(dst: &mut [u8], src: &[u8], width: usize) {
    let len = dst.len().min(src.len());
    dst[..len].copy_from_slice(&src[..len]);
    dst[len..].fill(0);
    if let Some(last) = dst.last_mut() {
        *last &= last_mask(width);
    }
}

#[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
#[inline]
fn encode_u64(out: &mut [MaybeUninit<u8>], value: u64) -> usize {
    use std::arch::x86_64::*;

    let bits = (64 - value.leading_zeros() as usize).max(1);
    let remaining = value << (64 - bits);
    let out = &mut out[..65];
    out[0].write(b'b');
    // SAFETY: SSE2 is enabled for this target. Each unaligned store initializes
    // exactly one 16-byte chunk of reserved capacity. Only the significant
    // digits are published; any extra initialized bytes remain outside len.
    unsafe {
        let masks = _mm_set1_epi64x(0x0102_0408_1020_4080);
        // Duplicate all eight input bytes once. Each chunk then selects two
        // adjacent bytes and repeats each eight times, high byte first.
        let word = _mm_cvtsi64_si128(remaining as i64);
        let pairs = _mm_unpacklo_epi8(word, word);
        let chunks = out[1..].as_chunks_mut::<16>().0;
        let emit = |chunk: &mut [MaybeUninit<u8>; 16], bytes| {
            let ones = _mm_cmpeq_epi8(_mm_and_si128(bytes, masks), masks);
            let ascii = _mm_sub_epi8(_mm_set1_epi8(b'0' as i8), ones);
            _mm_storeu_si128(chunk.as_mut_ptr().cast(), ascii);
        };
        emit(
            &mut chunks[0],
            _mm_shuffle_epi32::<0xfa>(_mm_shufflehi_epi16::<0xaf>(pairs)),
        );
        if bits > 16 {
            emit(
                &mut chunks[1],
                _mm_shuffle_epi32::<0xfa>(_mm_shufflehi_epi16::<0x05>(pairs)),
            );
        }
        if bits > 32 {
            emit(
                &mut chunks[2],
                _mm_shuffle_epi32::<0x50>(_mm_shufflelo_epi16::<0xaf>(pairs)),
            );
        }
        if bits > 48 {
            emit(
                &mut chunks[3],
                _mm_shuffle_epi32::<0x50>(_mm_shufflelo_epi16::<0x05>(pairs)),
            );
        }
    }
    1 + bits
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "sse2")))]
#[inline]
fn encode_u64(out: &mut [MaybeUninit<u8>], value: u64) -> usize {
    encode_value(out, 64, &value.to_le_bytes(), &[])
}

/// Initializes and returns the value's encoded length. The caller reserves a
/// full-width record, including any SIMD and suffix padding, before encoding.
fn encode_value(out: &mut [MaybeUninit<u8>], width: usize, value: &[u8], mask: &[u8]) -> usize {
    let prefix = usize::from(width != 1);
    if width != 1 {
        out[0].write(b'b');
    }
    let four_state = mask.iter().any(|&byte| byte != 0);
    let bits = if four_state {
        width
    } else {
        value
            .iter()
            .rposition(|&byte| byte != 0)
            .map_or(1, |i| i * 8 + (8 - value[i].leading_zeros() as usize))
    };
    let out = &mut out[prefix..prefix + bits];
    if !four_state {
        let bytes = bits.div_ceil(8);
        let high = value.get(bytes - 1).copied().unwrap_or(0);
        let high_bits = bits - (bytes - 1) * 8;
        copy_encoded(out, &BINARY[high as usize][8 - high_bits..]);
        for (chunk, &byte) in out[high_bits..]
            .as_chunks_mut::<8>()
            .0
            .iter_mut()
            .zip(value[..bytes - 1].iter().rev())
        {
            copy_encoded(chunk, &BINARY[byte as usize]);
        }
        return prefix + bits;
    }
    for (dst, bit) in out.iter_mut().zip((0..bits).rev()) {
        let v = value.get(bit / 8).copied().unwrap_or(0) >> (bit % 8) & 1;
        let m = mask.get(bit / 8).copied().unwrap_or(0) >> (bit % 8) & 1;
        dst.write(match (m, v) {
            (0, 0) => b'0',
            (0, _) => b'1',
            (_, 0) => b'z',
            _ => b'x',
        });
    }
    prefix + bits
}

#[inline]
fn copy_encoded(out: &mut [MaybeUninit<u8>], bytes: &[u8]) {
    let out = &mut out[..bytes.len()];
    // SAFETY: the checked destination has enough capacity and the borrowed
    // source cannot overlap it. This initializes exactly bytes.len() bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.as_mut_ptr().cast(), bytes.len());
    }
}

const BINARY: [[u8; 8]; 256] = {
    let mut table = [[b'0'; 8]; 256];
    let mut byte = 0;
    while byte < 256 {
        let mut bit = 0;
        while bit < 8 {
            table[byte][bit] += ((byte >> (7 - bit)) & 1) as u8;
            bit += 1;
        }
        byte += 1;
    }
    table
};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod encoding_tests;
