use celox_state_layout::{TRACE_GROUP_BYTES, get_byte_size};
use num_bigint::BigUint;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::mem::MaybeUninit;
use std::path::Path;

/// Describes a signal for VCD recording.
///
/// Self-contained — does not reference any IR types. Can be cached
/// alongside a shared backend artifact so that VCD
/// works even on cache-hit paths.
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug)]
pub struct VcdExternalSignalDesc {
    pub scope: String,
    pub name: String,
    pub width: usize,
}

#[derive(Clone, Copy)]
enum VcdWriterSource {
    Memory { offset: usize, is_4state: bool },
    External { index: usize },
    // Classify once so the dump loop can use a fixed-width load and store.
    MemoryBit { offset: usize },
    Memory64 { offset: usize },
}

struct VcdWriterSignal {
    suffix: VcdRecordSuffix,
    scope: String,
    name: String,
    width: usize,
    source: VcdWriterSource,
    previous_offset: usize,
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
    writer: BufWriter<W>,
    signals: Vec<VcdWriterSignal>,
    previous: Vec<u8>,
    initialized: Vec<bool>,
    groups: fxhash::FxHashMap<usize, Vec<usize>>,
    selected: Vec<usize>,
    activity: Vec<usize>,
    /// Complete value records accumulated for a bulk write. Successful dumps
    /// always hand the tail to BufWriter so flush and Drop own pending output.
    encoded: Vec<u8>,
    encoded_changes: u64,
    stats: VcdStatistics,
    timestamp: u64,
    header_written: bool,
    external_count: usize,
}

impl VcdWriter<File> {
    pub fn new<P: AsRef<Path>>(path: P, descs: &[VcdSignalDesc]) -> std::io::Result<Self> {
        Ok(Self::from_writer(File::create(path)?, descs))
    }
}

impl<W: Write> VcdWriter<W> {
    pub fn from_writer(writer: W, descs: &[VcdSignalDesc]) -> Self {
        let mut previous = Vec::new();
        let mut groups: fxhash::FxHashMap<usize, Vec<usize>> = Default::default();
        let signals = descs
            .iter()
            .enumerate()
            .map(|(index, desc)| {
                let previous_offset = previous.len();
                previous.resize(previous.len() + get_byte_size(desc.width) * 2, 0);
                let group = desc.offset / TRACE_GROUP_BYTES;
                groups.entry(group).or_default().push(index);
                VcdWriterSignal {
                    suffix: VcdRecordSuffix::new(desc.width, ""),
                    scope: desc.scope.clone(),
                    name: desc.name.clone(),
                    width: desc.width,
                    source: match (desc.width, desc.is_4state) {
                        (1, false) => VcdWriterSource::MemoryBit {
                            offset: desc.offset,
                        },
                        (64, false) => VcdWriterSource::Memory64 {
                            offset: desc.offset,
                        },
                        _ => VcdWriterSource::Memory {
                            offset: desc.offset,
                            is_4state: desc.is_4state,
                        },
                    },
                    previous_offset,
                }
            })
            .collect::<Vec<_>>();
        Self {
            writer: BufWriter::with_capacity(256 * 1024, writer),
            initialized: vec![false; signals.len()],
            signals,
            previous,
            groups,
            selected: Vec::new(),
            activity: Vec::new(),
            encoded: Vec::new(),
            encoded_changes: 0,
            stats: VcdStatistics::default(),
            timestamp: 0,
            header_written: false,
            external_count: 0,
        }
    }

    /// Publish buffered output, also reporting errors that Drop cannot report.
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }

    pub fn statistics(&self) -> VcdStatistics {
        self.stats
    }
    pub fn get_ref(&self) -> &W {
        self.writer.get_ref()
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
        let mut activity = std::mem::take(&mut self.activity);
        let tracked = backend.take_vcd_activity(&mut activity);
        let (ptr, size) = backend.memory_as_ptr();
        // SAFETY: the backend owns the image and cannot run during this dump.
        let memory = unsafe { std::slice::from_raw_parts(ptr, size) };
        let result =
            self.dump_with_activity(timestamp, memory, external, tracked.then_some(&activity));
        self.activity = activity;
        result
    }

    pub fn into_inner(mut self) -> std::io::Result<W> {
        self.flush()?;
        self.writer.into_inner().map_err(|error| error.into_error())
    }

    /// Adds externally supplied signals before the first dump. VCD headers
    /// cannot be extended after value changes have started.
    pub fn add_external_signals(&mut self, descs: &[VcdExternalSignalDesc]) -> std::io::Result<()> {
        if descs.is_empty() {
            return Ok(());
        }
        if self.external_count != 0 {
            let existing = self
                .signals
                .iter()
                .filter(|signal| matches!(signal.source, VcdWriterSource::External { .. }))
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
        if self.header_written {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cannot add external VCD signals after the first dump",
            ));
        }
        for desc in descs {
            let index = self.external_count;
            self.external_count += 1;
            self.signals.push(VcdWriterSignal {
                suffix: VcdRecordSuffix::new(desc.width, ""),
                scope: desc.scope.clone(),
                name: desc.name.clone(),
                width: desc.width,
                source: VcdWriterSource::External { index },
                previous_offset: self.previous.len(),
            });
            self.previous
                .resize(self.previous.len() + get_byte_size(desc.width) * 2, 0);
            self.initialized.push(false);
        }
        Ok(())
    }

    #[cold]
    fn write_header(&mut self) -> std::io::Result<()> {
        writeln!(self.writer, "$date")?;
        writeln!(
            self.writer,
            "  {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        )?;
        writeln!(self.writer, "$end")?;
        writeln!(self.writer, "$version")?;
        writeln!(self.writer, "  celox")?;
        writeln!(self.writer, "$end")?;
        writeln!(self.writer, "$timescale 1ns $end")?;

        let mut scope_order = Vec::<String>::new();
        let mut scope_groups = Vec::<Vec<usize>>::new();
        let mut scope_idx = fxhash::FxHashMap::<String, usize>::default();
        for (signal_index, signal) in self.signals.iter().enumerate() {
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
            writeln!(self.writer, "$scope module {} $end", scope)?;
            for signal_index in group {
                let signal = &mut self.signals[signal_index];
                let id = Self::generate_vcd_id(next_id);
                signal.suffix = VcdRecordSuffix::new(signal.width, &id);
                next_id += 1;
                writeln!(
                    self.writer,
                    "$var wire {} {} {} $end",
                    signal.width, id, signal.name
                )?;
            }
            writeln!(self.writer, "$upscope $end")?;
        }
        writeln!(self.writer, "$enddefinitions $end")?;
        writeln!(self.writer, "$dumpvars")?;
        writeln!(self.writer, "$end")?;
        if let Some(max_record) = self
            .signals
            .iter()
            .map(|signal| {
                signal.width.max(1) + usize::from(signal.width != 1) + signal.suffix.capacity()
            })
            .max()
        {
            // A block can cross the writer's capacity by one complete record.
            // Include the short suffix's padding, even for scalar-only traces.
            // Integer SIMD stores also fit within the full declared width.
            // Reserve here so record encoding never needs to grow the Vec.
            self.encoded.reserve(self.writer.capacity() + max_record);
        }
        self.header_written = true;
        Ok(())
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
        if external.len() != self.external_count {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "expected {} external VCD values, got {}",
                    self.external_count,
                    external.len()
                ),
            ));
        }
        let first_dump = !self.header_written;
        if first_dump {
            self.write_header()?;
        }
        if timestamp > self.timestamp || timestamp == 0 {
            writeln!(self.writer, "#{}", timestamp)?;
            self.timestamp = timestamp;
        }
        self.encoded.clear();
        self.encoded_changes = 0;
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
                .extend(self.signals.len() - self.external_count..self.signals.len());
            // Preserve registration order even when homes are laid out differently.
            self.selected.sort_unstable();
            self.selected.dedup();
        }
        let count = if sparse.is_some() {
            self.selected.len()
        } else {
            self.signals.len()
        };
        for index in 0..count {
            let i = if sparse.is_some() {
                self.selected[index]
            } else {
                index
            };
            let sig = &self.signals[i];
            self.stats.comparisons += 1;
            let value_len = match sig.source {
                VcdWriterSource::Memory { .. } | VcdWriterSource::External { .. } => {
                    let size = get_byte_size(sig.width);
                    // The memory path borrows bytes directly. External component values
                    // retain their existing BigUint ABI and are converted only here.
                    let external_bytes;
                    let (value, mask, track_mask): (&[u8], &[u8], bool) = match sig.source {
                        VcdWriterSource::MemoryBit { .. } | VcdWriterSource::Memory64 { .. } => {
                            unreachable!()
                        }
                        VcdWriterSource::Memory { offset, is_4state } => (
                            &memory[offset..offset + size],
                            if is_4state {
                                &memory[offset + size..offset + size * 2]
                            } else {
                                &[]
                            },
                            is_4state,
                        ),
                        VcdWriterSource::External { index } => {
                            external_bytes = (
                                external[index].0.to_bytes_le(),
                                external[index].1.to_bytes_le(),
                            );
                            (&external_bytes.0, &external_bytes.1, true)
                        }
                    };
                    let old =
                        &mut self.previous[sig.previous_offset..sig.previous_offset + size * 2];
                    if self.initialized[i]
                        && plane_equal(&old[..size], value, sig.width)
                        && (!track_mask || plane_equal(&old[size..], mask, sig.width))
                    {
                        continue;
                    }
                    copy_plane(&mut old[..size], value, sig.width);
                    // A two-state memory signal's previous mask stays zero for the
                    // writer's lifetime. External values can change between X/Z and
                    // known values, so their masks always participate.
                    if track_mask {
                        copy_plane(&mut old[size..], mask, sig.width);
                    }
                    self.initialized[i] = true;
                    encode_value(
                        self.encoded.spare_capacity_mut(),
                        sig.width,
                        &old[..size],
                        if track_mask { &old[size..] } else { &[] },
                    )
                }
                VcdWriterSource::MemoryBit { offset } => {
                    let value = memory[offset] & 1;
                    let old = &mut self.previous[sig.previous_offset];
                    if self.initialized[i] && *old == value {
                        continue;
                    }
                    *old = value;
                    self.initialized[i] = true;
                    self.encoded.spare_capacity_mut()[0].write(b'0' + value);
                    1
                }
                VcdWriterSource::Memory64 { offset } => {
                    let value: [u8; 8] = memory[offset..offset + 8].try_into().unwrap();
                    let old: &mut [u8; 8] = (&mut self.previous
                        [sig.previous_offset..sig.previous_offset + 8])
                        .try_into()
                        .unwrap();
                    if self.initialized[i] && u64::from_ne_bytes(*old) == u64::from_ne_bytes(value)
                    {
                        continue;
                    }
                    *old = value;
                    self.initialized[i] = true;
                    encode_u64(self.encoded.spare_capacity_mut(), u64::from_le_bytes(value))
                }
            };
            let suffix_len = sig
                .suffix
                .encode(&mut self.encoded.spare_capacity_mut()[value_len..]);
            // SAFETY: the encoders initialize their returned lengths in checked
            // slices of spare capacity. Publish only the complete record, once;
            // any SIMD/suffix padding stays outside the Vec's visible length.
            unsafe {
                self.encoded
                    .set_len(self.encoded.len() + value_len + suffix_len);
            }
            self.encoded_changes += 1;
            if self.encoded.len() >= self.writer.capacity() {
                self.write_encoded()?;
            }
        }
        self.write_encoded()
    }

    fn write_encoded(&mut self) -> std::io::Result<()> {
        if !self.encoded.is_empty() {
            // Full blocks bypass BufWriter's internal copy. A dump's final
            // partial block stays buffered with the timestamps and header.
            self.writer.write_all(&self.encoded)?;
            self.stats.changes += self.encoded_changes;
            self.stats.value_bytes += self.encoded.len() as u64;
            self.encoded.clear();
            self.encoded_changes = 0;
        }
        Ok(())
    }
}

fn last_mask(width: usize) -> u8 {
    if width.is_multiple_of(8) {
        0xff
    } else {
        ((1u16 << (width % 8)) - 1) as u8
    }
}

fn plane_equal(old: &[u8], value: &[u8], width: usize) -> bool {
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

fn copy_plane(dst: &mut [u8], src: &[u8], width: usize) {
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
mod tests {
    use super::*;

    #[test]
    fn external_values_are_masked_to_their_declared_width() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("external-width.vcd");
        let mut writer = VcdWriter::new(&path, &[]).unwrap();
        writer
            .add_external_signals(&[VcdExternalSignalDesc {
                scope: "component".into(),
                name: "state".into(),
                width: 8,
            }])
            .unwrap();

        writer
            .dump_with_external(
                0,
                &[],
                &[(BigUint::from(0x1ffu16), BigUint::from(0x100u16))],
            )
            .unwrap();
        writer
            .dump_with_external(1, &[], &[(BigUint::from(0xffu8), BigUint::default())])
            .unwrap();
        writer
            .dump_with_external(2, &[], &[(BigUint::default(), BigUint::from(0xffu8))])
            .unwrap();
        writer
            .dump_with_external(3, &[], &[(BigUint::default(), BigUint::default())])
            .unwrap();

        writer.flush().unwrap();
        let dump = std::fs::read_to_string(path).unwrap();
        assert!(!dump.contains("b111111111"), "{dump}");
        assert_eq!(dump.matches("b11111111 !").count(), 1, "{dump}");
        assert_eq!(dump.matches("bzzzzzzzz !").count(), 1, "{dump}");
        assert_eq!(dump.matches("b0 !").count(), 1, "{dump}");
    }
}

#[cfg(test)]
mod encoding_tests {
    use super::*;

    fn changes(bytes: &[u8]) -> Vec<(u64, String)> {
        let mut parser = vcd::Parser::new(bytes);
        parser.parse_header().unwrap();
        let mut time = 0;
        parser
            .filter_map(|command| match command.unwrap() {
                vcd::Command::Timestamp(t) => {
                    time = t;
                    None
                }
                vcd::Command::ChangeScalar(_, value) => Some((time, value.to_string())),
                vcd::Command::ChangeVector(_, value) => Some((time, value.to_string())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn integer_encoding_matches_binary_format_at_every_bit_length() {
        let mut values = vec![0, u64::MAX, 0x0123_4567_89ab_cdef, 0xaaaa_5555_aaaa_5555];
        let mut random = 0x8314_40be_9d6a_2785u64;
        for bit in 0..64 {
            let mask = u64::MAX >> (63 - bit);
            values.extend([1 << bit, mask]);
            for _ in 0..32 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                values.push((random & mask) | (1 << bit));
            }
        }
        // Exercise unaligned output, capacity growth, and reuse of spare bytes
        // after truncation, checking that neither prefixes nor lengths change.
        for prefix in [0, 1, 15, 16, 63] {
            let mut actual = vec![b'#'; prefix];
            let mut expected = actual.clone();
            for &value in &values {
                actual.reserve(65);
                let len = encode_u64(actual.spare_capacity_mut(), value);
                // SAFETY: encode_u64 initialized len bytes of spare capacity.
                unsafe { actual.set_len(actual.len() + len) };
                expected.extend_from_slice(format!("b{value:b}").as_bytes());
                assert_eq!(actual, expected, "prefix={prefix} value={value:#x}");
                if actual.len() > 4096 {
                    actual.truncate(prefix);
                    expected.truncate(prefix);
                }
            }
        }
    }

    #[test]
    fn record_suffixes_preserve_ids_and_never_publish_padding() {
        for (number, expected) in [(0, "!"), (93, "~"), (94, "!!"), (8929, "~~"), (8930, "!!!")] {
            assert_eq!(VcdWriter::<Vec<u8>>::generate_vcd_id(number), expected);
        }
        // Test both sides of the inline boundary without allocating the huge
        // signal set needed to reach long IDs through normal registration.
        for id in [
            "!",
            "~",
            "!!",
            "~~~",
            "abcdef",
            "abcdefg",
            "abcdefgh",
            "abcdefghi",
            "abcdefghijklmnop",
        ] {
            for width in [1, 64] {
                let suffix = VcdRecordSuffix::new(width, id);
                let expected_suffix = format!("{}{id}\n", if width == 1 { "" } else { " " });
                for prefix in 0..16 {
                    let mut guarded = [MaybeUninit::new(b'#'); 64];
                    let end = prefix + suffix.capacity();
                    let len = suffix.encode(&mut guarded[prefix..end]);
                    // SAFETY: all guard bytes started initialized, and encoding
                    // only overwrites them with initialized suffix bytes.
                    let actual = guarded.map(|byte| unsafe { byte.assume_init() });
                    assert_eq!(&actual[..prefix], vec![b'#'; prefix]);
                    assert_eq!(&actual[prefix..prefix + len], expected_suffix.as_bytes());
                    assert!(actual[end..].iter().all(|&byte| byte == b'#'));

                    let mut records = vec![b'#'; prefix];
                    let mut expected = records.clone();
                    for value in [0u64, 1, 1 << 63, u64::MAX, 0] {
                        let value_capacity = if width == 1 { 1 } else { 65 };
                        records.reserve(value_capacity + suffix.capacity());
                        // Bound the spare slice to exactly the reserved record
                        // space, including fixed-store padding. Later records
                        // must overwrite that padding at the visible length.
                        let out =
                            &mut records.spare_capacity_mut()[..value_capacity + suffix.capacity()];
                        let value_len = if width == 1 {
                            out[0].write(b'0' + (value & 1) as u8);
                            1
                        } else {
                            encode_u64(out, value)
                        };
                        let suffix_len = suffix.encode(&mut out[value_len..]);
                        // SAFETY: both encoders initialized their returned lengths.
                        unsafe { records.set_len(records.len() + value_len + suffix_len) };
                        let value_text = if width == 1 {
                            (value & 1).to_string()
                        } else {
                            format!("b{value:b}")
                        };
                        expected.extend_from_slice(value_text.as_bytes());
                        expected.extend_from_slice(expected_suffix.as_bytes());
                        assert_eq!(records, expected, "id={id} width={width} prefix={prefix}");
                    }
                }
            }
        }
    }

    #[test]
    fn complete_records_cross_small_blocks_with_scalar_and_mixed_widths() {
        for scalar_only in [true, false] {
            let descs = (0..100)
                .map(|i| VcdSignalDesc {
                    scope: format!("scope{}", i % 2),
                    name: format!("s{i}"),
                    offset: i * 8,
                    width: if scalar_only { 1 } else { [1, 9, 64][i % 3] },
                    is_4state: false,
                })
                .collect::<Vec<_>>();
            for capacity in [1, 3, 7, 8, 9, 16, 64, 71, 72, 73] {
                let mut writer = VcdWriter::from_writer(Vec::new(), &descs);
                writer.writer = BufWriter::with_capacity(capacity, Vec::new());
                for (time, value) in [0xff, 0, 0, 0xff].into_iter().enumerate() {
                    writer.dump(time as u64, &[value; 800]).unwrap();
                }
                assert_eq!(writer.statistics().changes, 300);
                let bytes = writer.into_inner().unwrap();
                let mut parser = vcd::Parser::new(bytes.as_slice());
                let header = parser.parse_header().unwrap();
                let mut names = fxhash::FxHashMap::default();
                for item in header.items {
                    if let vcd::ScopeItem::Scope(scope) = item {
                        for item in scope.items {
                            if let vcd::ScopeItem::Var(var) = item {
                                names.insert(var.code, var.reference);
                            }
                        }
                    }
                }
                assert_eq!(names.len(), descs.len());
                let mut time = 0;
                let actual = parser
                    .filter_map(|command| {
                        let (id, value) = match command.unwrap() {
                            vcd::Command::Timestamp(t) => {
                                time = t;
                                return None;
                            }
                            vcd::Command::ChangeScalar(id, value) => (id, value.to_string()),
                            vcd::Command::ChangeVector(id, value) => (id, value.to_string()),
                            _ => return None,
                        };
                        Some((time, names[&id].clone(), value))
                    })
                    .collect::<Vec<_>>();
                let expected = [0, 1, 3]
                    .into_iter()
                    .flat_map(|time| {
                        descs.iter().map(move |desc| {
                            let value = if time == 1 {
                                "0".into()
                            } else {
                                "1".repeat(desc.width)
                            };
                            (time, desc.name.clone(), value)
                        })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    actual, expected,
                    "scalar_only={scalar_only} capacity={capacity}"
                );
            }
        }
    }

    #[test]
    fn integer_memory_at_unaligned_buffer_end_preserves_changes_and_aliases() {
        for offset in 1..=8 {
            let descs = [
                VcdSignalDesc {
                    scope: "top".into(),
                    name: "prefix".into(),
                    offset: 0,
                    width: 7,
                    is_4state: false,
                },
                VcdSignalDesc {
                    scope: "top".into(),
                    name: "q".into(),
                    offset,
                    width: 64,
                    is_4state: false,
                },
                VcdSignalDesc {
                    scope: "top".into(),
                    name: "alias".into(),
                    offset,
                    width: 64,
                    is_4state: false,
                },
            ];
            let mut writer = VcdWriter::from_writer(Vec::new(), &descs);
            // Both previous values follow a 7-bit signal, so their cached
            // offsets are unaligned too. No padding follows the memory value.
            let mut memory = vec![0; offset + 8];
            memory[0] = 0x45;
            let mut expected = vec![(0, format!("{:b}", memory[0]))];
            for (step, value) in std::iter::once(0u64)
                .chain((0..64).map(|bit| 1 << bit))
                .chain([u64::MAX, 0])
                .enumerate()
            {
                let time = (step * 2) as u64;
                memory[offset..].copy_from_slice(&value.to_le_bytes());
                writer.dump(time, &memory).unwrap();
                writer.dump(time + 1, &memory).unwrap();
                expected.extend(std::iter::repeat_n((time, format!("{value:b}")), 2));
            }
            assert_eq!(writer.statistics().changes, expected.len() as u64);
            assert_eq!(
                changes(&writer.into_inner().unwrap()),
                expected,
                "offset={offset}"
            );
        }
    }

    #[test]
    fn emitted_values_match_independent_biguint_oracle() {
        for width in [1, 7, 8, 9, 31, 32, 63, 64, 65, 257, 1024] {
            for four_state in [false, true] {
                let desc = VcdSignalDesc {
                    scope: "top".into(),
                    name: "q".into(),
                    offset: 0,
                    width,
                    is_4state: four_state,
                };
                let mut writer = VcdWriter::from_writer(Vec::new(), &[desc]);
                let size = width.div_ceil(8);
                let mut memory = vec![0; size * 2];
                let limit = (BigUint::from(1u8) << width) - 1u8;
                let mut expected = vec![];
                let mut previous = None;
                let mut random = 0x8314_40be_9d6a_2785u64;
                for step in 0..96u64 {
                    if step % 4 != 1 {
                        for byte in &mut memory {
                            random ^= random << 13;
                            random ^= random >> 7;
                            random ^= random << 17;
                            *byte = random as u8;
                        }
                    }
                    if step % 3 == 0 {
                        memory[size..].fill(0);
                    }
                    if step % 8 == 0 {
                        memory[..size].fill(0);
                    }
                    // Deliberately include nonzero padding above the declared width.
                    let value = BigUint::from_bytes_le(&memory[..size]) & &limit;
                    let mask = if four_state {
                        BigUint::from_bytes_le(&memory[size..]) & &limit
                    } else {
                        BigUint::default()
                    };
                    let state = (value.clone(), mask.clone());
                    if previous.as_ref() != Some(&state) {
                        let text = if mask == BigUint::default() {
                            value.to_str_radix(2)
                        } else {
                            (0..width)
                                .rev()
                                .map(|bit| match (mask.bit(bit as u64), value.bit(bit as u64)) {
                                    (false, false) => '0',
                                    (false, true) => '1',
                                    (true, false) => 'z',
                                    (true, true) => 'x',
                                })
                                .collect()
                        };
                        expected.push((step / 2, text));
                        previous = Some(state);
                    }
                    writer.dump(step / 2, &memory).unwrap();
                }
                assert_eq!(
                    changes(&writer.into_inner().unwrap()),
                    expected,
                    "width={width} four_state={four_state}"
                );
            }
        }
    }

    #[test]
    fn sparse_selection_preserves_aliases_initial_values_and_registration_order() {
        let descs = [128, 0, 129, 0, 512, 640, 768, 896, 1024, 1152]
            .into_iter()
            .enumerate()
            .map(|(index, offset)| VcdSignalDesc {
                scope: "top".into(),
                name: format!("s{index}"),
                offset,
                width: 8,
                is_4state: false,
            })
            .collect::<Vec<_>>();
        let mut sparse = VcdWriter::from_writer(Vec::new(), &descs);
        let mut full = VcdWriter::from_writer(Vec::new(), &descs);
        let mut memory = vec![0; 1153];
        for time in 0..5 {
            memory[0] = time as u8;
            memory[129] = (time * 3) as u8;
            // First sample ignores an empty candidate set; later sets arrive unsorted.
            let groups = if time == 0 { &[][..] } else { &[2, 0, 2][..] };
            sparse
                .dump_with_activity(time, &memory, &[], Some(groups))
                .unwrap();
            full.dump(time, &memory).unwrap();
        }
        assert!(sparse.statistics().comparisons < full.statistics().comparisons);
        let parse = |bytes: Vec<u8>| {
            let mut parser = vcd::Parser::new(bytes.as_slice());
            parser.parse_header().unwrap();
            parser.map(Result::unwrap).collect::<Vec<_>>()
        };
        assert_eq!(
            parse(sparse.into_inner().unwrap()),
            parse(full.into_inner().unwrap())
        );
    }

    #[test]
    fn padding_and_mask_only_changes_have_exact_semantics() {
        let desc = VcdSignalDesc {
            scope: "top".into(),
            name: "q".into(),
            offset: 0,
            width: 1,
            is_4state: true,
        };
        let mut writer = VcdWriter::from_writer(Vec::new(), &[desc]);
        for (time, bytes) in [[0, 0], [0xfe, 0xfe], [0, 1], [1, 1], [1, 0]]
            .iter()
            .enumerate()
        {
            writer.dump(time as u64, bytes).unwrap();
        }
        assert_eq!(
            changes(&writer.into_inner().unwrap()),
            [(0, "0"), (2, "z"), (3, "x"), (4, "1")].map(|(t, s)| (t, s.to_string()))
        );
    }

    #[derive(Default)]
    struct ShortWrites {
        bytes: Vec<u8>,
        calls: usize,
        fail_after: Option<usize>,
    }

    impl Write for ShortWrites {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls.is_multiple_of(7) {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let remaining = self
                .fail_after
                .unwrap_or(usize::MAX)
                .saturating_sub(self.bytes.len());
            if remaining == 0 && !bytes.is_empty() {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            let len = bytes.len().min(113).min(remaining);
            self.bytes.extend_from_slice(&bytes[..len]);
            Ok(len)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocks_and_oversized_records_survive_short_writes_and_drop() {
        let mut offset = 0;
        let descs = (0..1025)
            .map(|i| {
                let width = if i == 1024 {
                    256 * 1024 + 17
                } else {
                    [1, 9, 65, 1024][i % 4]
                };
                let is_4state = i % 3 == 0 || i == 1024;
                let desc = VcdSignalDesc {
                    scope: "top".into(),
                    name: format!("s{i}"),
                    offset,
                    width,
                    is_4state,
                };
                offset += width.div_ceil(8) * if is_4state { 2 } else { 1 };
                desc
            })
            .collect::<Vec<_>>();
        let mut memory = vec![0xff; offset];
        let mut sink = ShortWrites::default();
        let mut expected = Vec::new();
        let stats = {
            let mut writer = VcdWriter::from_writer(&mut sink, &descs);
            writer
                .add_external_signals(&[VcdExternalSignalDesc {
                    scope: "component".into(),
                    name: "state".into(),
                    width: 9,
                }])
                .unwrap();
            for time in 0..4 {
                if time == 1 {
                    memory.fill(0);
                } else if time == 3 {
                    for desc in &descs {
                        let size = desc.width.div_ceil(8);
                        if desc.is_4state {
                            memory[desc.offset + size..desc.offset + size * 2].fill(0xff);
                        } else {
                            memory[desc.offset..desc.offset + size].fill(0xff);
                        }
                    }
                }
                let external = match time {
                    0 => (BigUint::from(0x1ffu16), BigUint::from(0x1ffu16)),
                    3 => (BigUint::default(), BigUint::from(0x1ffu16)),
                    _ => (BigUint::default(), BigUint::default()),
                };
                writer
                    .dump_with_external(time, &memory, &[external])
                    .unwrap();
                if time != 2 {
                    for desc in &descs {
                        let value = if time == 1 {
                            "0".into()
                        } else if desc.is_4state {
                            if time == 0 { "x" } else { "z" }.repeat(desc.width)
                        } else {
                            "1".repeat(desc.width)
                        };
                        expected.push((time, value));
                    }
                    expected.push((
                        time,
                        match time {
                            0 => "x".repeat(9),
                            3 => "z".repeat(9),
                            _ => "0".into(),
                        },
                    ));
                }
            }
            // Leave the final partial block buffered and exercise Drop.
            writer.statistics()
        };
        assert_eq!(changes(&sink.bytes), expected);
        let text = std::str::from_utf8(&sink.bytes).unwrap();
        assert!(text.contains("\n#2\n#3\n"));
        assert_eq!(stats.changes, expected.len() as u64);
        let values = text.split_once("$dumpvars\n$end\n").unwrap().1;
        assert_eq!(
            stats.value_bytes,
            values
                .lines()
                .filter(|line| !line.starts_with('#'))
                .map(|line| (line.len() + 1) as u64)
                .sum::<u64>()
        );
    }

    #[test]
    fn block_and_tail_io_errors_are_reported() {
        for (width, fail_after, fails_during_dump) in
            [(64, 16, false), (512 * 1024, 256 * 1024 + 13, true)]
        {
            let desc = VcdSignalDesc {
                scope: "top".into(),
                name: "q".into(),
                offset: 0,
                width,
                is_4state: false,
            };
            let mut sink = ShortWrites {
                fail_after: Some(fail_after),
                ..Default::default()
            };
            {
                let mut writer = VcdWriter::from_writer(&mut sink, &[desc]);
                let dump = writer.dump(0, &vec![0xff; width.div_ceil(8)]);
                assert_eq!(dump.is_err(), fails_during_dump);
                let error = dump.and_then(|()| writer.flush()).unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
            }
            assert_eq!(sink.bytes.len(), fail_after);
        }
    }

    #[test]
    fn explicit_flush_reports_sink_failure() {
        struct BadFlush;
        impl Write for BadFlush {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("flush failed"))
            }
        }
        let mut writer = VcdWriter::from_writer(BadFlush, &[]);
        writer.dump(0, &[]).unwrap();
        assert_eq!(writer.flush().unwrap_err().to_string(), "flush failed");
    }
}
