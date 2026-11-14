//! FST (Fast Signal Trace) waveform output.
//!
//! Mirrors the [`VcdWriter`](crate::VcdWriter) recording contract — signal
//! descriptions, activity-based change detection, checkpoints, byte limits
//! and restarts — while encoding through the pure-Rust `wavefst` crate.
//!
//! An FST file header carries the final timestamp and section count, so the
//! writer patches it on every [`FstWriter::flush`] and again when finished
//! (in [`FstWriter::into_inner`], or best-effort on drop like `BufWriter`,
//! where errors cannot be reported). A flushed file is readable mid-run.
//!
//! The encoder is `wavefst` rather than `fst-writer`: the latter cannot
//! flush except right before a time step (it panics) and produces an empty
//! time table for a run that only ever dumps at time zero, which waveform
//! readers built on `wellen`/`fst-reader` reject.
use crate::vcd::{VcdExternalSignalDesc, VcdSignalDesc, VcdStatistics, copy_plane, plane_equal};
use celox_state_layout::TRACE_GROUP_BYTES;
use num_bigint::BigUint;
use std::borrow::Cow;
use std::fs::File;
use std::io::{self, Seek, Write};
use std::path::Path;
use std::sync::Arc;
#[expect(
    clippy::disallowed_types,
    reason = "single-threaded: byte count shared between FstWriter and the CountingWriter that wavefst owns, both written and read on the simulation thread"
)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use wavefst::{
    ChainCompression, FileType, GeomEntry, Header, HierarchyCompression, ScopeType, SignalValue,
    VarDir, VarType,
};

/// Queued value payload bound handed to the encoder; it flushes a
/// value-change block once a dump crosses it, bounding writer memory.
const BLOCK_BYTE_LIMIT: usize = 16 * 1024 * 1024;

struct HeaderSignal {
    scope: String,
    name: String,
    width: usize,
}

#[derive(Clone, Copy)]
enum Source {
    Memory { offset: usize, is_4state: bool },
    External { index: usize },
}

#[derive(Clone, Copy)]
struct SignalState {
    source: Source,
    width: usize,
    previous_offset: usize,
}

/// Counts bytes handed to the underlying writer so `$dumplimit` can be
/// approximated while `fst-writer` owns the stream.
struct CountingWriter<W> {
    inner: W,
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: byte count shared between FstWriter and the CountingWriter that wavefst owns, both written and read on the simulation thread"
    )]
    bytes: Arc<AtomicU64>,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(buf)?;
        self.bytes.fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> Seek for CountingWriter<W> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

fn fst_error(error: wavefst::Error) -> io::Error {
    match error {
        wavefst::Error::Io(io) => io,
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}

pub struct FstWriter<W: Write + Seek = File> {
    /// Destination held until the header starts at the first dump, so
    /// externally supplied signals can still join the hierarchy.
    pending_output: Option<W>,
    body: Option<wavefst::FstWriter<CountingWriter<W>>>,
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: byte count shared between FstWriter and the CountingWriter that wavefst owns, both written and read on the simulation thread"
    )]
    bytes_written: Arc<AtomicU64>,
    closed: bool,
    headers: Vec<HeaderSignal>,
    signals: Vec<SignalState>,
    /// Value plane followed by mask plane for every signal, last emitted.
    previous: Vec<u8>,
    ids: Vec<Option<u32>>,
    groups: fxhash::FxHashMap<usize, Vec<usize>>,
    selected: Vec<usize>,
    activity: Vec<usize>,
    rescan_pending: bool,
    descs: Vec<VcdSignalDesc>,
    external_descs: Vec<VcdExternalSignalDesc>,
    external_count: usize,
    memory_required: usize,
    last_timestamp: Option<u64>,
    initial_values_written: bool,
    suspended: bool,
    byte_limit: Option<u64>,
    time_precision: celox_design::TimePrecision,
    scratch: Vec<u8>,
    stats: VcdStatistics,
}

impl FstWriter<File> {
    pub fn new<P: AsRef<Path>>(path: P, descs: &[VcdSignalDesc]) -> io::Result<Self> {
        Self::new_with_time_precision(path, descs, celox_design::TimePrecision::default())
    }

    pub fn new_with_time_precision<P: AsRef<Path>>(
        path: P,
        descs: &[VcdSignalDesc],
        time_precision: celox_design::TimePrecision,
    ) -> io::Result<Self> {
        Ok(Self::from_writer_with_time_precision(
            File::create(path)?,
            descs,
            time_precision,
        ))
    }

    /// Finish the current file and continue the waveform, with the same
    /// signals, in a new file at `path`. The first dump there records every
    /// value, and its timestamps start over.
    pub fn restart<P: AsRef<Path>>(&mut self, path: P) -> io::Result<()> {
        self.finalize()?;
        let mut next = Self::new_with_time_precision(path, &self.descs, self.time_precision)?;
        next.add_external_signals(&self.external_descs)?;
        next.suspended = self.suspended;
        next.byte_limit = self.byte_limit;
        *self = next;
        Ok(())
    }
}

impl<W: Write + Seek> FstWriter<W> {
    pub fn from_writer(writer: W, descs: &[VcdSignalDesc]) -> Self {
        Self::from_writer_with_time_precision(writer, descs, celox_design::TimePrecision::default())
    }

    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: byte count shared between FstWriter and the CountingWriter that wavefst owns, both written and read on the simulation thread"
    )]
    pub fn from_writer_with_time_precision(
        writer: W,
        descs: &[VcdSignalDesc],
        time_precision: celox_design::TimePrecision,
    ) -> Self {
        let mut headers = Vec::with_capacity(descs.len());
        let mut signals = Vec::with_capacity(descs.len());
        let mut groups: fxhash::FxHashMap<usize, Vec<usize>> = Default::default();
        let mut previous = Vec::new();
        let mut memory_required = 0usize;
        for (index, desc) in descs.iter().enumerate() {
            let size = desc.width.div_ceil(8);
            let end = size
                .checked_mul(if desc.is_4state { 2 } else { 1 })
                .and_then(|size| desc.offset.checked_add(size))
                .expect("FST signal memory range overflows");
            memory_required = memory_required.max(end);
            groups
                .entry(desc.offset / TRACE_GROUP_BYTES)
                .or_default()
                .push(index);
            headers.push(HeaderSignal {
                scope: desc.scope.clone(),
                name: desc.name.clone(),
                width: desc.width,
            });
            signals.push(SignalState {
                source: Source::Memory {
                    offset: desc.offset,
                    is_4state: desc.is_4state,
                },
                width: desc.width,
                previous_offset: previous.len(),
            });
            previous.resize(previous.len() + size * 2, 0);
        }
        Self {
            pending_output: Some(writer),
            body: None,
            bytes_written: Arc::new(AtomicU64::new(0)),
            closed: false,
            ids: vec![None; headers.len()],
            headers,
            signals,
            previous,
            groups,
            selected: Vec::new(),
            activity: Vec::new(),
            rescan_pending: false,
            descs: descs.to_vec(),
            external_descs: Vec::new(),
            external_count: 0,
            memory_required,
            last_timestamp: None,
            initial_values_written: false,
            suspended: false,
            byte_limit: None,
            time_precision,
            scratch: Vec::new(),
            stats: VcdStatistics::default(),
        }
    }

    pub fn statistics(&self) -> VcdStatistics {
        self.stats
    }

    pub fn last_timestamp(&self) -> Option<u64> {
        self.last_timestamp
    }

    /// Make the next [`Self::dump_backend`] compare every signal: memory
    /// changed without marking activity, for example by restoring a
    /// checkpoint.
    pub fn rescan(&mut self) {
        self.rescan_pending = true;
    }

    /// Set the maximum output size requested by `$dumplimit`. Zero removes
    /// the limit. The size is approximated by bytes written plus the
    /// encoder's buffered estimate, checked at dump boundaries.
    pub fn set_byte_limit(&mut self, limit: u64) {
        if self.limit_reached() {
            self.rescan_pending = true;
            self.initial_values_written = false;
        }
        self.byte_limit = (limit != 0).then_some(limit);
    }

    fn output_size(&self) -> u64 {
        // Queued but unflushed value payload is bounded by BLOCK_BYTE_LIMIT
        // and not observable here, so the limit can overshoot by that much.
        self.bytes_written.load(Ordering::Relaxed)
    }

    fn limit_reached(&self) -> bool {
        self.byte_limit
            .is_some_and(|limit| self.output_size() >= limit)
    }

    fn ensure_body(&mut self) -> io::Result<()> {
        if self.body.is_some() {
            return Ok(());
        }
        let out = self
            .pending_output
            .take()
            .expect("pending output held until the header starts");
        let counting = CountingWriter {
            inner: out,
            bytes: Arc::clone(&self.bytes_written),
        };
        let mut header = wavefst::FstWriter::builder(counting)
            .timescale_exponent(i32::from(self.time_precision) as i8)
            .chain_compression(ChainCompression::Lz4)
            .hierarchy_compression(HierarchyCompression::Lz4)
            .block_size_limit(BLOCK_BYTE_LIMIT)
            .build()
            .map_err(fst_error)?;
        // Group signals by scope in first-seen order, as the VCD header does.
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
        for (scope, group) in scope_order.iter().zip(scope_groups) {
            header
                .begin_scope(ScopeType::VcdModule, scope.clone(), None)
                .map_err(fst_error)?;
            for signal_index in group {
                let signal = &self.headers[signal_index];
                let width = u32::try_from(signal.width.max(1)).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("FST signal width {} exceeds the format limit", signal.width),
                    )
                })?;
                let id = header
                    .add_variable(
                        VarType::VcdWire,
                        VarDir::Implicit,
                        signal.name.clone(),
                        GeomEntry::Fixed(width),
                    )
                    .map_err(fst_error)?;
                self.ids[signal_index] = Some(id);
            }
            header.end_scope().map_err(fst_error)?;
        }
        header
            .write_header(Header {
                version: "celox".to_string(),
                date: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                file_type: FileType::Verilog,
                ..Header::default()
            })
            .map_err(fst_error)?;
        self.body = Some(header);
        Ok(())
    }

    /// Finalize the file (patching its header) and return the destination.
    /// After this call the writer records nothing further.
    fn finalize(&mut self) -> io::Result<Option<W>> {
        if self.closed {
            return Ok(None);
        }
        self.ensure_body()?;
        self.closed = true;
        let body = self.body.take().expect("body started by ensure_body");
        let counting = body.finish().map_err(fst_error)?;
        let mut inner = counting.inner;
        inner.flush()?;
        Ok(Some(inner))
    }

    /// Publish buffered value changes and patch the file header, making
    /// the file readable before the run ends.
    pub fn flush(&mut self) -> io::Result<()> {
        if let Some(body) = self.body.as_mut() {
            body.flush().map_err(fst_error)?;
        }
        Ok(())
    }

    pub fn into_inner(mut self) -> io::Result<W> {
        self.finalize()?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "FST writer already finished")
        })
    }

    /// Adds externally supplied signals before the first dump. FST headers
    /// cannot be extended after value changes have started.
    pub fn add_external_signals(&mut self, descs: &[VcdExternalSignalDesc]) -> io::Result<()> {
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
        if self.body.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot add external FST signals after the first dump",
            ));
        }
        self.external_descs.extend_from_slice(descs);
        for desc in descs {
            let index = self.external_count;
            self.external_count += 1;
            self.headers.push(HeaderSignal {
                scope: desc.scope.clone(),
                name: desc.name.clone(),
                width: desc.width,
            });
            self.signals.push(SignalState {
                source: Source::External { index },
                width: desc.width,
                previous_offset: self.previous.len(),
            });
            self.previous
                .resize(self.previous.len() + desc.width.div_ceil(8) * 2, 0);
            self.ids.push(None);
        }
        Ok(())
    }

    fn validate_external_count(&self, count: usize) -> io::Result<()> {
        if count != self.external_count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "expected {} external FST values, got {count}",
                    self.external_count
                ),
            ));
        }
        Ok(())
    }

    fn begin_timestamp(&mut self, timestamp: u64) -> io::Result<()> {
        if let Some(last) = self.last_timestamp
            && timestamp < last
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "FST timestamp {timestamp} is earlier than the last dumped timestamp {last}; start a new file to record a rewound simulation"
                ),
            ));
        }
        self.ensure_body()?;
        self.last_timestamp = Some(timestamp);
        Ok(())
    }

    fn emit_signal(
        &mut self,
        index: usize,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
        force: bool,
    ) -> io::Result<()> {
        self.stats.comparisons += 1;
        let state = self.signals[index];
        let size = state.width.div_ceil(8);
        let external_bytes;
        let (value, mask, track_mask): (&[u8], &[u8], bool) = match state.source {
            Source::Memory { offset, is_4state } => (
                &memory[offset..offset + size],
                if is_4state {
                    &memory[offset + size..offset + size * 2]
                } else {
                    &[]
                },
                is_4state,
            ),
            Source::External { index } => {
                external_bytes = (
                    external[index].0.to_bytes_le(),
                    external[index].1.to_bytes_le(),
                );
                (&external_bytes.0, &external_bytes.1, true)
            }
        };
        let old = &mut self.previous[state.previous_offset..state.previous_offset + size * 2];
        if !force
            && plane_equal(&old[..size], value, state.width)
            && (!track_mask || plane_equal(&old[size..], mask, state.width))
        {
            return Ok(());
        }
        copy_plane(&mut old[..size], value, state.width);
        if track_mask {
            copy_plane(&mut old[size..], mask, state.width);
        }
        if state.width == 0 {
            return Ok(());
        }
        // FST values are full-width bit strings, most significant bit first,
        // using the VCD plane convention: a masked bit set in the value
        // plane is `x`, clear is `z`.
        self.scratch.clear();
        self.scratch.resize(state.width, 0);
        for (out, bit) in self.scratch.iter_mut().zip((0..state.width).rev()) {
            let v = old.get(bit / 8).copied().unwrap_or(0) >> (bit % 8) & 1;
            let m = if track_mask {
                old[size + bit / 8] >> (bit % 8) & 1
            } else {
                0
            };
            *out = match (m, v) {
                (0, 0) => b'0',
                (0, _) => b'1',
                (_, 0) => b'z',
                _ => b'x',
            };
        }
        let id = self.ids[index].expect("header started before emission");
        let timestamp = self
            .last_timestamp
            .expect("timestamp begun before emission");
        let body = self.body.as_mut().expect("header started before emission");
        // The scratch holds only `0`, `1`, `x` and `z` bytes by construction.
        let value = std::str::from_utf8(&self.scratch).expect("ASCII bit string");
        body.emit_change(timestamp, id, SignalValue::Vector(Cow::Borrowed(value)))
            .map_err(fst_error)?;
        self.stats.changes += 1;
        self.stats.value_bytes += state.width as u64;
        Ok(())
    }

    fn emit_all(
        &mut self,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
        selected: Option<Vec<usize>>,
        force: bool,
    ) -> io::Result<()> {
        match selected {
            Some(selected) => {
                for index in selected {
                    if self.limit_reached() {
                        break;
                    }
                    self.emit_signal(index, memory, external, force)?;
                }
            }
            None => {
                for index in 0..self.signals.len() {
                    if self.limit_reached() {
                        break;
                    }
                    self.emit_signal(index, memory, external, force)?;
                }
            }
        }
        Ok(())
    }

    fn memory_end(&self, state: SignalState) -> usize {
        match state.source {
            Source::Memory { offset, is_4state } => {
                offset + state.width.div_ceil(8) * if is_4state { 2 } else { 1 }
            }
            Source::External { .. } => 0,
        }
    }

    /// Emit a SystemVerilog checkpoint, mirroring the VCD writer: `dumpoff`
    /// writes unknowns and suspends changes; `dumpon` writes current values
    /// and resumes; `dumpall` writes current values without changing
    /// suspension. FST has no checkpoint markers, only the values.
    pub fn checkpoint(
        &mut self,
        command: &str,
        timestamp: u64,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
    ) -> io::Result<()> {
        if !matches!(command, "dumpoff" | "dumpon" | "dumpall") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid FST checkpoint",
            ));
        }
        self.validate_external_count(external.len())?;
        match command {
            "dumpoff" => self.suspended = true,
            "dumpon" => self.suspended = false,
            _ => {}
        }
        if self.limit_reached() {
            return Ok(());
        }
        self.begin_timestamp(timestamp)?;
        if command == "dumpoff" {
            for index in 0..self.signals.len() {
                if self.limit_reached() {
                    break;
                }
                let width = self.headers[index].width.max(1);
                let id = self.ids[index].expect("header started by begin_timestamp");
                self.scratch.clear();
                self.scratch.resize(width, b'x');
                let body = self.body.as_mut().expect("header started");
                let value = std::str::from_utf8(&self.scratch).expect("ASCII bit string");
                body.emit_change(timestamp, id, SignalValue::Vector(Cow::Borrowed(value)))
                    .map_err(fst_error)?;
                self.stats.changes += 1;
                self.stats.value_bytes += width as u64;
            }
            self.body
                .as_mut()
                .expect("header started")
                .emit_dump_active(timestamp, false)
                .map_err(fst_error)?;
        } else {
            self.emit_all(memory, external, None, true)?;
            self.initial_values_written = true;
            if command == "dumpon" {
                self.body
                    .as_mut()
                    .expect("header started")
                    .emit_dump_active(timestamp, true)
                    .map_err(fst_error)?;
            }
        }
        self.rescan_pending = true;
        Ok(())
    }

    /// Full-scan reference path, also suitable for uninstrumented/raw memory.
    pub fn dump(&mut self, timestamp: u64, memory: &[u8]) -> io::Result<()> {
        self.dump_with_external(timestamp, memory, &[])
    }

    pub fn dump_with_external(
        &mut self,
        timestamp: u64,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
    ) -> io::Result<()> {
        self.dump_with_activity(timestamp, memory, external, None)
    }

    /// Dump as the backend's sole incremental waveform observer, consuming
    /// its pending activity like [`VcdWriter::dump_backend`](crate::VcdWriter::dump_backend).
    pub fn dump_backend<B: crate::backend::SimBackend>(
        &mut self,
        timestamp: u64,
        backend: &mut B,
        external: &[(BigUint, BigUint)],
    ) -> io::Result<()> {
        self.validate_external_count(external.len())?;
        let mut activity = std::mem::take(&mut self.activity);
        let tracked = if activity.is_empty() {
            backend.take_vcd_activity(&mut activity)
        } else {
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

    /// `activity` contains unique physical group IDs consumed from
    /// TraceLayout. None requests a full scan. Initial values and external
    /// signals are always observed.
    pub fn dump_with_activity(
        &mut self,
        timestamp: u64,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
        activity: Option<&[usize]>,
    ) -> io::Result<()> {
        self.validate_external_count(external.len())?;
        if self.suspended || self.limit_reached() {
            return Ok(());
        }
        let first_dump = !self.initial_values_written;
        self.begin_timestamp(timestamp)?;
        self.selected.clear();
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
            self.selected.sort_unstable();
            self.selected.dedup();
            if self.selected.is_empty() {
                return Ok(());
            }
            for &index in &self.selected {
                assert!(
                    self.memory_end(self.signals[index]) <= memory.len(),
                    "FST memory does not cover a selected signal"
                );
            }
        } else if memory.len() < self.memory_required {
            panic!("FST memory does not cover all signals");
        }
        let selected = sparse.map(|_| std::mem::take(&mut self.selected));
        self.emit_all(memory, external, selected, first_dump)?;
        self.initial_values_written = true;
        Ok(())
    }
}

impl<W: Write + Seek> Drop for FstWriter<W> {
    fn drop(&mut self) {
        // An FST file without its patched header is unreadable, so finish
        // best-effort like `BufWriter` flushes on drop.
        let _ = self.finalize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn desc(
        scope: &str,
        name: &str,
        offset: usize,
        width: usize,
        is_4state: bool,
    ) -> VcdSignalDesc {
        VcdSignalDesc {
            scope: scope.into(),
            name: name.into(),
            offset,
            width,
            is_4state,
        }
    }

    /// Read a finished FST back with an independent parser and return the
    /// `(time, bit string)` changes of one signal.
    fn changes(bytes: Vec<u8>, scope: &str, name: &str) -> Vec<(u64, String)> {
        let mut wave =
            wellen::simple::read_from_reader(Cursor::new(bytes)).expect("wellen reads the FST");
        let hierarchy = wave.hierarchy();
        let var = hierarchy
            .lookup_var(&[scope], name)
            .unwrap_or_else(|| panic!("{scope}.{name} in hierarchy"));
        let signal_ref = hierarchy[var].signal_ref();
        wave.load_signals(&[signal_ref]);
        let times: Vec<u64> = wave.time_table().to_vec();
        wave.get_signal(signal_ref)
            .expect("signal loaded")
            .iter_changes()
            .map(|(index, value)| {
                (
                    times[index as usize],
                    value.to_bit_string().expect("bit vector value"),
                )
            })
            .collect()
    }

    fn write_basic() -> Vec<u8> {
        // clk: 1 bit at 0; data: 8 bits at 8 (4-state: value at 8, mask at 9).
        let descs = [
            desc("Top", "clk", 0, 1, false),
            desc("Top", "data", 8, 8, true),
        ];
        let mut writer = FstWriter::from_writer(Cursor::new(Vec::new()), &descs);
        let mut memory = vec![0u8; 16];
        writer.dump(0, &memory).unwrap();
        memory[0] = 1;
        memory[8] = 0b1010_1100;
        writer.dump(10, &memory).unwrap();
        // Unchanged dump records nothing new; a flush in between is deferred
        // to the next timestamp and must not corrupt the section stream.
        writer.flush().unwrap();
        writer.dump(20, &memory).unwrap();
        // data becomes x/z: mask bit with value 1 is x, with value 0 is z.
        memory[8] = 0b0000_0011;
        memory[9] = 0b0000_0101;
        writer.dump(30, &memory).unwrap();
        writer.into_inner().unwrap().into_inner()
    }

    #[test]
    fn round_trips_values_through_wellen() {
        let bytes = write_basic();
        assert_eq!(
            changes(bytes.clone(), "Top", "clk"),
            [(0, "0".to_string()), (10, "1".to_string())]
        );
        assert_eq!(
            changes(bytes, "Top", "data"),
            [
                (0, "00000000".to_string()),
                (10, "10101100".to_string()),
                (30, "00000z1x".to_string()),
            ]
        );
    }

    #[test]
    fn records_the_design_timescale() {
        let descs = [desc("Top", "clk", 0, 1, false)];
        let precision = celox_design::TimePrecision::try_from(-9).unwrap();
        let mut writer =
            FstWriter::from_writer_with_time_precision(Cursor::new(Vec::new()), &descs, precision);
        writer.dump(0, &[0u8; 8]).unwrap();
        let bytes = writer.into_inner().unwrap().into_inner();
        let wave = wellen::simple::read_from_reader(Cursor::new(bytes)).unwrap();
        let timescale = wave.hierarchy().timescale().expect("timescale recorded");
        assert_eq!(timescale.unit.to_exponent(), Some(-9));
        assert_eq!(timescale.factor, 1);
    }

    #[test]
    fn drop_finalizes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drop.fst");
        let descs = [desc("Top", "clk", 0, 1, false)];
        {
            let mut writer = FstWriter::new(&path, &descs).unwrap();
            writer.dump(0, &[1u8; 8]).unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(changes(bytes, "Top", "clk"), [(0, "1".to_string())]);
    }

    #[test]
    fn checkpoints_write_unknowns_and_resume() {
        let descs = [desc("Top", "value", 0, 4, false)];
        let mut writer = FstWriter::from_writer(Cursor::new(Vec::new()), &descs);
        let memory = [0b0101u8; 8];
        writer.dump(0, &memory).unwrap();
        writer.checkpoint("dumpoff", 10, &memory, &[]).unwrap();
        writer.dump(20, &[0b1111; 8]).unwrap(); // suspended: not recorded
        writer.checkpoint("dumpon", 30, &memory, &[]).unwrap();
        let bytes = writer.into_inner().unwrap().into_inner();
        assert_eq!(
            changes(bytes, "Top", "value"),
            [
                (0, "0101".to_string()),
                (10, "xxxx".to_string()),
                (30, "0101".to_string()),
            ]
        );
    }

    #[test]
    fn rejects_decreasing_timestamps() {
        let descs = [desc("Top", "clk", 0, 1, false)];
        let mut writer = FstWriter::from_writer(Cursor::new(Vec::new()), &descs);
        writer.dump(10, &[0u8; 8]).unwrap();
        let error = writer.dump(5, &[0u8; 8]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("earlier"), "{error}");
    }

    #[test]
    fn external_signals_join_the_header_before_the_first_dump() {
        let descs = [desc("Top", "clk", 0, 1, false)];
        let mut writer = FstWriter::from_writer(Cursor::new(Vec::new()), &descs);
        writer
            .add_external_signals(&[VcdExternalSignalDesc {
                scope: "Ext".into(),
                name: "probe".into(),
                width: 4,
            }])
            .unwrap();
        writer
            .dump_with_external(0, &[0u8; 8], &[(BigUint::from(9u8), BigUint::default())])
            .unwrap();
        assert!(
            writer
                .add_external_signals(&[VcdExternalSignalDesc {
                    scope: "Ext".into(),
                    name: "late".into(),
                    width: 1,
                }])
                .is_err(),
            "header is fixed after the first dump"
        );
        let bytes = writer.into_inner().unwrap().into_inner();
        assert_eq!(changes(bytes, "Ext", "probe"), [(0, "1001".to_string())]);
    }

    #[test]
    fn byte_limit_stops_recording() {
        let descs = [desc("Top", "value", 0, 8, false)];
        let mut writer = FstWriter::from_writer(Cursor::new(Vec::new()), &descs);
        writer.dump(0, &[3u8; 16]).unwrap();
        // The header alone exceeds one byte, so later dumps record nothing;
        // crucially the finished file must stay parseable.
        writer.set_byte_limit(1);
        writer.dump(10, &[7u8; 16]).unwrap();
        let bytes = writer.into_inner().unwrap().into_inner();
        assert_eq!(
            changes(bytes, "Top", "value"),
            [(0, "00000011".to_string())]
        );
    }

    #[test]
    fn restart_starts_a_fresh_file() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.fst");
        let second = dir.path().join("second.fst");
        let descs = [desc("Top", "clk", 0, 1, false)];
        let mut writer = FstWriter::new(&first, &descs).unwrap();
        writer.dump(0, &[0u8; 8]).unwrap();
        writer.dump(10, &[1u8; 8]).unwrap();
        writer.restart(&second).unwrap();
        writer.dump(0, &[1u8; 8]).unwrap();
        drop(writer);
        assert_eq!(
            changes(std::fs::read(&first).unwrap(), "Top", "clk"),
            [(0, "0".to_string()), (10, "1".to_string())]
        );
        assert_eq!(
            changes(std::fs::read(&second).unwrap(), "Top", "clk"),
            [(0, "1".to_string())]
        );
    }
}
