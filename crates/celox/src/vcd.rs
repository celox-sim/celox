use crate::ir::{AbsoluteAddr, Program};
use malachite_bigint::BigUint;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

pub struct VcdWriter {
    writer: BufWriter<File>,
    id_map: HashMap<AbsoluteAddr, (String, usize, bool)>,
    signal_order: Vec<AbsoluteAddr>,
    last_values: HashMap<AbsoluteAddr, (BigUint, BigUint)>,
    timestamp: u64,
}

impl VcdWriter {
    pub fn new<P: AsRef<Path>>(path: P, program: &Program) -> std::io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        let mut id_map = HashMap::default();
        let mut next_id_num = 0;
        let mut signal_order = Vec::new();

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

        // Hierarchical signal definitions
        // We need to group signals by instance
        let mut instance_signals: HashMap<
            crate::ir::InstanceId,
            Vec<(String, AbsoluteAddr, usize, bool)>,
        > = HashMap::default();

        for (instance_id, module_id) in &program.instance_module {
            let variables = &program.module_variables[module_id];
            for (var_path, info) in variables {
                let addr = AbsoluteAddr {
                    instance_id: *instance_id,
                    var_id: info.id,
                };
                let name = var_path
                    .0
                    .iter()
                    .map(|s| {
                        veryl_parser::resource_table::get_str_value(*s)
                            .unwrap()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                instance_signals
                    .entry(*instance_id)
                    .or_default()
                    .push((name, addr, info.width, info.is_4state));
            }
        }

        // Write hierarchy
        let mut sorted_instances: Vec<_> = instance_signals.iter().collect();
        sorted_instances.sort_by_key(|(id, _)| *id);

        for (inst_id, signals) in sorted_instances {
            writeln!(writer, "$scope module {} $end", inst_id)?;
            let mut sorted_signals = signals.clone();
            sorted_signals.sort_by(|a, b| a.0.cmp(&b.0));

            for (name, addr, width, is_4state) in sorted_signals {
                let vcd_id = Self::generate_vcd_id(next_id_num);
                next_id_num += 1;
                writeln!(writer, "$var wire {} {} {} $end", width, vcd_id, name)?;
                id_map.insert(addr, (vcd_id, width, is_4state));
                signal_order.push(addr);
            }
            writeln!(writer, "$upscope $end")?;
        }

        writeln!(writer, "$enddefinitions $end")?;
        writeln!(writer, "$dumpvars")?;
        writeln!(writer, "$end")?;

        Ok(Self {
            writer,
            id_map,
            signal_order,
            last_values: HashMap::default(),
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

    pub fn dump(
        &mut self,
        timestamp: u64,
        get_val: impl Fn(&AbsoluteAddr) -> (BigUint, BigUint),
    ) -> std::io::Result<()> {
        if timestamp > self.timestamp || timestamp == 0 {
            writeln!(self.writer, "#{}", timestamp)?;
            self.timestamp = timestamp;
        }

        for addr in &self.signal_order {
            let &(ref vcd_id, width, is_4state) = &self.id_map[addr];
            let (current_val, current_mask) = get_val(addr);
            let prev = self.last_values.get(addr);

            let changed = match prev {
                Some((pv, pm)) => pv != &current_val || pm != &current_mask,
                None => true,
            };

            if changed {
                if is_4state && current_mask != BigUint::from(0u32) {
                    // 4-state output with x/z characters
                    Self::write_four_state_value(
                        &mut self.writer,
                        width,
                        &current_val,
                        &current_mask,
                        vcd_id,
                    )?;
                } else if width == 1 {
                    writeln!(self.writer, "{}{}", current_val, vcd_id)?;
                } else {
                    writeln!(
                        self.writer,
                        "b{} {}",
                        current_val.to_str_radix(2),
                        vcd_id
                    )?;
                }
                self.last_values
                    .insert(*addr, (current_val, current_mask));
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
