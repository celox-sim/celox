use crate::backend::get_byte_size;
use num_bigint::BigUint;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Describes a signal for VCD recording.
///
/// Self-contained — does not reference any IR types. Can be cached
/// alongside [`SharedJitCode`](crate::backend::SharedJitCode) so that VCD
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

struct VcdWriterSignal {
    vcd_id: String,
    offset: usize,
    width: usize,
    is_4state: bool,
}

pub struct VcdWriter {
    writer: BufWriter<File>,
    signals: Vec<VcdWriterSignal>,
    last_values: Vec<Option<(BigUint, BigUint)>>,
    timestamp: u64,
}

impl VcdWriter {
    pub fn new<P: AsRef<Path>>(path: P, descs: &[VcdSignalDesc]) -> std::io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        // VCD Header
        writeln!(writer, "$date")?;
        writeln!(
            writer,
            "  {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        )?;
        writeln!(writer, "$end")?;
        writeln!(writer, "$version")?;
        writeln!(writer, "  celox")?;
        writeln!(writer, "$end")?;
        writeln!(writer, "$timescale 1ns $end")?;

        // Group signals by scope, preserving insertion order.
        let mut scope_order: Vec<&str> = Vec::new();
        let mut scope_groups: Vec<Vec<&VcdSignalDesc>> = Vec::new();
        let mut scope_idx: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();

        for desc in descs {
            match scope_idx.get(desc.scope.as_str()) {
                Some(&idx) => scope_groups[idx].push(desc),
                None => {
                    scope_idx.insert(&desc.scope, scope_order.len());
                    scope_order.push(&desc.scope);
                    scope_groups.push(vec![desc]);
                }
            }
        }

        // Write hierarchy and assign VCD IDs
        let mut signals = Vec::with_capacity(descs.len());
        let mut next_id_num = 0usize;

        for (i, scope_name) in scope_order.iter().enumerate() {
            writeln!(writer, "$scope module {} $end", scope_name)?;
            for desc in &scope_groups[i] {
                let vcd_id = Self::generate_vcd_id(next_id_num);
                next_id_num += 1;
                writeln!(
                    writer,
                    "$var wire {} {} {} $end",
                    desc.width, vcd_id, desc.name
                )?;
                signals.push(VcdWriterSignal {
                    vcd_id,
                    offset: desc.offset,
                    width: desc.width,
                    is_4state: desc.is_4state,
                });
            }
            writeln!(writer, "$upscope $end")?;
        }

        writeln!(writer, "$enddefinitions $end")?;
        writeln!(writer, "$dumpvars")?;
        writeln!(writer, "$end")?;

        let last_values = vec![None; signals.len()];

        Ok(Self {
            writer,
            signals,
            last_values,
            timestamp: 0,
        })
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

    /// Dump all changed signals at the given timestamp.
    ///
    /// `memory` is the raw JIT memory (stable region or full buffer).
    pub fn dump(&mut self, timestamp: u64, memory: &[u8]) -> std::io::Result<()> {
        if timestamp > self.timestamp || timestamp == 0 {
            writeln!(self.writer, "#{}", timestamp)?;
            self.timestamp = timestamp;
        }

        for (i, sig) in self.signals.iter().enumerate() {
            let byte_size = get_byte_size(sig.width);
            let current_val = Self::read_value(memory, sig.offset, sig.width);
            let current_mask = if sig.is_4state {
                Self::read_value(memory, sig.offset + byte_size, sig.width)
            } else {
                BigUint::from(0u32)
            };

            let prev = &self.last_values[i];
            let changed = match prev {
                Some((pv, pm)) => pv != &current_val || pm != &current_mask,
                None => true,
            };

            if changed {
                if sig.is_4state && current_mask != BigUint::from(0u32) {
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
