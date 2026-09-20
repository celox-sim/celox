//! Registration-order runs for the hot waveform comparison loop.
use super::{
    VcdOutput, VcdRecordSuffix, VcdSignalDesc, copy_plane, encode_u64, encode_value, plane_equal,
};
use num_bigint::BigUint;
use std::io::Write;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bit,
    Word,
    Generic,
}

#[derive(Clone, Copy)]
struct Entry {
    kind: Kind,
    index: usize,
}

#[derive(Clone, Copy)]
struct Run {
    kind: Kind,
    start: usize,
    end: usize,
}

struct FixedSignal {
    suffix: VcdRecordSuffix,
    offset: usize,
    /// Last encoded value; VcdOutput retains its record until the write succeeds.
    previous: u64,
}

enum GenericSource {
    Memory { offset: usize, is_4state: bool },
    External { index: usize },
}

struct GenericSignal {
    suffix: VcdRecordSuffix,
    source: GenericSource,
    width: usize,
    previous_offset: usize,
}

#[derive(Default)]
pub(super) struct TracePlan {
    entries: Vec<Entry>,
    runs: Vec<Run>,
    bits: Vec<FixedSignal>,
    words: Vec<FixedSignal>,
    generic: Vec<GenericSignal>,
    /// Includes values queued in VcdOutput, even after a partial write fails.
    previous: Vec<u8>,
    memory_required: usize,
}

impl TracePlan {
    pub(super) fn add_memory(&mut self, desc: &VcdSignalDesc) {
        let size = desc.width.div_ceil(8);
        let end = size
            .checked_mul(if desc.is_4state { 2 } else { 1 })
            .and_then(|size| desc.offset.checked_add(size))
            .expect("VCD signal memory range overflows");
        self.memory_required = self.memory_required.max(end);
        let suffix = VcdRecordSuffix::new(desc.width, "");
        let entry = match (desc.width, desc.is_4state) {
            (1 | 64, false) => {
                let (kind, signals) = if desc.width == 1 {
                    (Kind::Bit, &mut self.bits)
                } else {
                    (Kind::Word, &mut self.words)
                };
                let index = signals.len();
                signals.push(FixedSignal {
                    suffix,
                    offset: desc.offset,
                    previous: 0,
                });
                Entry { kind, index }
            }
            _ => {
                let index = self.generic.len();
                self.generic.push(GenericSignal {
                    suffix,
                    source: GenericSource::Memory {
                        offset: desc.offset,
                        is_4state: desc.is_4state,
                    },
                    width: desc.width,
                    previous_offset: self.previous.len(),
                });
                self.previous.resize(self.previous.len() + size * 2, 0);
                Entry {
                    kind: Kind::Generic,
                    index,
                }
            }
        };
        self.push(entry);
    }

    pub(super) fn add_external(&mut self, index: usize, width: usize) {
        let entry = Entry {
            kind: Kind::Generic,
            index: self.generic.len(),
        };
        self.generic.push(GenericSignal {
            suffix: VcdRecordSuffix::new(width, ""),
            source: GenericSource::External { index },
            width,
            previous_offset: self.previous.len(),
        });
        self.previous
            .resize(self.previous.len() + width.div_ceil(8) * 2, 0);
        self.push(entry);
    }

    fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
        if let Some(run) = self.runs.last_mut()
            && run.kind == entry.kind
            && run.end == entry.index
        {
            run.end += 1;
        } else {
            self.runs.push(Run {
                kind: entry.kind,
                start: entry.index,
                end: entry.index + 1,
            });
        }
    }

    pub(super) fn suffix(&mut self, index: usize) -> &mut VcdRecordSuffix {
        let entry = self.entries[index];
        match entry.kind {
            Kind::Bit => &mut self.bits[entry.index].suffix,
            Kind::Word => &mut self.words[entry.index].suffix,
            Kind::Generic => &mut self.generic[entry.index].suffix,
        }
    }

    fn memory_end(&self, entry: Entry) -> usize {
        // Memory ranges were checked for overflow at registration.
        match entry.kind {
            Kind::Bit => self.bits[entry.index].offset + 1,
            Kind::Word => self.words[entry.index].offset + 8,
            Kind::Generic => {
                let signal = &self.generic[entry.index];
                match signal.source {
                    GenericSource::Memory { offset, is_4state } => {
                        offset + signal.width.div_ceil(8) * if is_4state { 2 } else { 1 }
                    }
                    GenericSource::External { .. } => 0,
                }
            }
        }
    }

    pub(super) fn dump<const INITIAL: bool, W: Write>(
        &mut self,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
        selected: Option<&[usize]>,
        output: &mut VcdOutput<W>,
    ) -> std::io::Result<()> {
        // Validate once per dump. A sparse caller may supply a shorter slice
        // that still covers every selected signal; preserve that contract.
        if memory.len() < self.memory_required {
            if let Some(selected) = selected {
                for &index in selected {
                    assert!(
                        self.memory_end(self.entries[index]) <= memory.len(),
                        "VCD memory does not cover a selected signal"
                    );
                }
            } else {
                panic!("VCD memory does not cover all signals");
            }
        }
        if let Some(selected) = selected {
            // Sparse indices can break a long typed run into many short ones.
            // Visit each entry once instead of first rebuilding those runs.
            for &index in selected {
                let entry = self.entries[index];
                let run = Run {
                    kind: entry.kind,
                    start: entry.index,
                    end: entry.index + 1,
                };
                // SAFETY: all selected memory ranges were validated above.
                unsafe {
                    self.write_run::<INITIAL, W>(run, memory, external, output)?;
                }
            }
        } else if self.runs.len() > self.entries.len() / 2 {
            // Alternating types form many tiny runs. Visit their entries
            // directly so each specialized loop has a known length of one.
            for index in 0..self.entries.len() {
                let entry = self.entries[index];
                let run = Run {
                    kind: entry.kind,
                    start: entry.index,
                    end: entry.index + 1,
                };
                // SAFETY: the complete memory range was validated above.
                unsafe {
                    self.write_run::<INITIAL, W>(run, memory, external, output)?;
                }
            }
        } else {
            for index in 0..self.runs.len() {
                // SAFETY: the complete memory range was validated above.
                unsafe {
                    self.write_run::<INITIAL, W>(self.runs[index], memory, external, output)?;
                }
            }
        }
        output.write_encoded()
    }

    /// Every signal in the run must fit in memory.
    // Runs can contain a single signal when types alternate. Keep dispatch in
    // the caller so these runs do not pay for a function prologue per signal.
    #[inline(always)]
    unsafe fn write_run<const INITIAL: bool, W: Write>(
        &mut self,
        run: Run,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
        output: &mut VcdOutput<W>,
    ) -> std::io::Result<()> {
        match run.kind {
            // SAFETY: the caller validated the selected memory ranges.
            Kind::Bit => unsafe {
                write_fixed::<INITIAL, true, W>(&mut self.bits[run.start..run.end], memory, output)
            },
            // SAFETY: the caller validated the selected memory ranges.
            Kind::Word => unsafe {
                write_fixed::<INITIAL, false, W>(
                    &mut self.words[run.start..run.end],
                    memory,
                    output,
                )
            },
            Kind::Generic => {
                let signals = &self.generic[run.start..run.end];
                for (index, signal) in signals.iter().enumerate() {
                    let size = signal.width.div_ceil(8);
                    let external_bytes;
                    let (value, mask, track_mask): (&[u8], &[u8], bool) = match signal.source {
                        GenericSource::Memory { offset, is_4state } => (
                            &memory[offset..offset + size],
                            if is_4state {
                                &memory[offset + size..offset + size * 2]
                            } else {
                                &[]
                            },
                            is_4state,
                        ),
                        GenericSource::External { index } => {
                            external_bytes = (
                                external[index].0.to_bytes_le(),
                                external[index].1.to_bytes_le(),
                            );
                            (&external_bytes.0, &external_bytes.1, true)
                        }
                    };
                    let old = &mut self.previous
                        [signal.previous_offset..signal.previous_offset + size * 2];
                    if !INITIAL
                        && plane_equal(&old[..size], value, signal.width)
                        && (!track_mask || plane_equal(&old[size..], mask, signal.width))
                    {
                        continue;
                    }
                    copy_plane(&mut old[..size], value, signal.width);
                    if track_mask {
                        copy_plane(&mut old[size..], mask, signal.width);
                    }
                    let len = encode_value(
                        output.encoded.spare_capacity_mut(),
                        signal.width,
                        &old[..size],
                        if track_mask { &old[size..] } else { &[] },
                    );
                    if let Err(error) = output.finish_record(len, &signal.suffix) {
                        output.stats.comparisons += index as u64 + 1;
                        return Err(error);
                    }
                }
                output.stats.comparisons += signals.len() as u64;
                Ok(())
            }
        }
    }
}

/// Every signal's one/eight-byte input must fit in memory. Checked by TracePlan.
#[inline(always)]
unsafe fn write_fixed<const INITIAL: bool, const BIT: bool, W: Write>(
    signals: &mut [FixedSignal],
    memory: &[u8],
    output: &mut VcdOutput<W>,
) -> std::io::Result<()> {
    for (index, signal) in signals.iter_mut().enumerate() {
        let value = if BIT {
            // SAFETY: the plan validated this one-byte range for this dump.
            u64::from(unsafe { *memory.as_ptr().add(signal.offset) } & 1)
        } else {
            // SAFETY: the plan validated all eight bytes; homes may be unaligned.
            u64::from_le(unsafe {
                memory
                    .as_ptr()
                    .add(signal.offset)
                    .cast::<u64>()
                    .read_unaligned()
            })
        };
        if !INITIAL && value == signal.previous {
            continue;
        }
        signal.previous = value;
        let len = if BIT {
            output.encoded.spare_capacity_mut()[0].write(b'0' + value as u8);
            1
        } else {
            encode_u64(output.encoded.spare_capacity_mut(), value)
        };
        if let Err(error) = output.finish_record(len, &signal.suffix) {
            output.stats.comparisons += index as u64 + 1;
            return Err(error);
        }
    }
    // Count a completed run once, avoiding a memory RMW dependency on every
    // comparison. The error path above counts exactly the visited prefix.
    output.stats.comparisons += signals.len() as u64;
    Ok(())
}
