use celox_state_layout::get_byte_size;
use num_bigint::BigUint;
use std::fs::File;
use std::io::{BufWriter, Write};
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
}

struct VcdWriterSignal {
    vcd_id: String,
    scope: String,
    name: String,
    width: usize,
    source: VcdWriterSource,
}

pub struct VcdWriter {
    writer: BufWriter<File>,
    signals: Vec<VcdWriterSignal>,
    last_values: Vec<Option<(BigUint, BigUint)>>,
    timestamp: u64,
    header_written: bool,
    external_count: usize,
}

impl VcdWriter {
    pub fn new<P: AsRef<Path>>(path: P, descs: &[VcdSignalDesc]) -> std::io::Result<Self> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        let signals = descs
            .iter()
            .map(|desc| VcdWriterSignal {
                vcd_id: String::new(),
                scope: desc.scope.clone(),
                name: desc.name.clone(),
                width: desc.width,
                source: VcdWriterSource::Memory {
                    offset: desc.offset,
                    is_4state: desc.is_4state,
                },
            })
            .collect::<Vec<_>>();

        let last_values = vec![None; signals.len()];

        Ok(Self {
            writer,
            signals,
            last_values,
            timestamp: 0,
            header_written: false,
            external_count: 0,
        })
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
                vcd_id: String::new(),
                scope: desc.scope.clone(),
                name: desc.name.clone(),
                width: desc.width,
                source: VcdWriterSource::External { index },
            });
            self.last_values.push(None);
        }
        Ok(())
    }

    fn write_header(&mut self) -> std::io::Result<()> {
        if self.header_written {
            return Ok(());
        }
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
                signal.vcd_id = Self::generate_vcd_id(next_id);
                next_id += 1;
                writeln!(
                    self.writer,
                    "$var wire {} {} {} $end",
                    signal.width, signal.vcd_id, signal.name
                )?;
            }
            writeln!(self.writer, "$upscope $end")?;
        }
        writeln!(self.writer, "$enddefinitions $end")?;
        writeln!(self.writer, "$dumpvars")?;
        writeln!(self.writer, "$end")?;
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

    /// Read a value from the JIT memory at the given offset and width.
    fn read_value(memory: &[u8], offset: usize, width: usize) -> BigUint {
        let byte_size = get_byte_size(width);
        let slice = &memory[offset..offset + byte_size];
        let mut val = BigUint::from_bytes_le(slice);
        let extra_bits = byte_size * 8 - width;
        if extra_bits > 0 {
            let mask = (BigUint::from(1u32) << width) - 1u32;
            val &= mask;
        }
        val
    }

    fn mask_to_width(mut value: BigUint, width: usize) -> BigUint {
        if value.bits() > width as u64 {
            value &= (BigUint::from(1u8) << width) - 1u8;
        }
        value
    }

    /// Dump all changed signals at the given timestamp.
    ///
    /// `memory` is the raw JIT memory (stable region or full buffer).
    pub fn dump(&mut self, timestamp: u64, memory: &[u8]) -> std::io::Result<()> {
        self.dump_with_external(timestamp, memory, &[])
    }

    /// Dump memory-backed signals and external values in registration order.
    pub fn dump_with_external(
        &mut self,
        timestamp: u64,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
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
        self.write_header()?;
        if timestamp > self.timestamp || timestamp == 0 {
            writeln!(self.writer, "#{}", timestamp)?;
            self.timestamp = timestamp;
        }

        for (i, sig) in self.signals.iter().enumerate() {
            let (current_val, current_mask, is_4state) = match sig.source {
                VcdWriterSource::Memory { offset, is_4state } => {
                    let byte_size = get_byte_size(sig.width);
                    let value = Self::read_value(memory, offset, sig.width);
                    let mask = if is_4state {
                        Self::read_value(memory, offset + byte_size, sig.width)
                    } else {
                        BigUint::from(0u32)
                    };
                    (value, mask, is_4state)
                }
                VcdWriterSource::External { index } => {
                    let (value, mask) = &external[index];
                    let value = Self::mask_to_width(value.clone(), sig.width);
                    let mask = Self::mask_to_width(mask.clone(), sig.width);
                    let is_4state = mask != BigUint::default();
                    (value, mask, is_4state)
                }
            };

            let prev = &self.last_values[i];
            let changed = match prev {
                Some((pv, pm)) => pv != &current_val || pm != &current_mask,
                None => true,
            };

            if changed {
                if is_4state && current_mask != BigUint::from(0u32) {
                    Self::write_four_state_value(
                        &mut self.writer,
                        sig.width,
                        &current_val,
                        &current_mask,
                        &sig.vcd_id,
                    )?;
                } else if sig.width == 1 {
                    writeln!(self.writer, "{}{}", current_val, sig.vcd_id)?;
                } else {
                    writeln!(
                        self.writer,
                        "b{} {}",
                        current_val.to_str_radix(2),
                        sig.vcd_id
                    )?;
                }
                self.last_values[i] = Some((current_val, current_mask));
            }
        }
        self.writer.flush()?;
        Ok(())
    }

    fn write_four_state_value(
        writer: &mut BufWriter<File>,
        width: usize,
        value: &BigUint,
        mask: &BigUint,
        vcd_id: &str,
    ) -> std::io::Result<()> {
        if width == 1 {
            let m = mask.bit(0);
            let v = value.bit(0);
            let ch = match (m, v) {
                (false, false) => '0',
                (false, true) => '1',
                (true, false) => 'z',
                (true, true) => 'x',
            };
            writeln!(writer, "{}{}", ch, vcd_id)
        } else {
            write!(writer, "b")?;
            for i in (0..width).rev() {
                let m = mask.bit(i as u64);
                let v = value.bit(i as u64);
                let ch = match (m, v) {
                    (false, false) => '0',
                    (false, true) => '1',
                    (true, false) => 'z',
                    (true, true) => 'x',
                };
                write!(writer, "{}", ch)?;
            }
            writeln!(writer, " {}", vcd_id)
        }
    }
}

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

        let dump = std::fs::read_to_string(path).unwrap();
        assert!(!dump.contains("b111111111"), "{dump}");
        assert_eq!(dump.matches("b11111111 !").count(), 1, "{dump}");
    }
}
