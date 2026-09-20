//! See docs/internals/vcd-performance.md for controls and interpretation.
use celox_runtime::{VcdSignalDesc, VcdWriter};
use celox_state_layout::{
    LayoutInput, LayoutSource, MemoryLayout, MemoryLayoutMode, StateObjectLayout, TraceLayout,
};
use std::{hint::black_box, io::Write, time::Instant};

struct Fixture {
    signals: usize,
    width: usize,
    four_state: bool,
}
impl LayoutSource<usize> for Fixture {
    fn layout_input(&self, _: MemoryLayoutMode) -> LayoutInput<usize> {
        LayoutInput {
            state_objects: (0..self.signals)
                .map(|address| StateObjectLayout {
                    address,
                    width: self.width,
                    is_4state: self.four_state,
                })
                .collect(),
            working_addresses: vec![],
            sparse_addresses: vec![],
            unpacked_arrays: Default::default(),
            requirements: Default::default(),
            ff_referenced_addresses: Default::default(),
            num_events: 0,
            runtime_event_sites: vec![],
        }
    }
}
struct Sink {
    file: Option<std::fs::File>,
    bytes: u64,
}
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        // Observe the actual encoded buffer even in the no-I/O measurement.
        black_box(bytes);
        let count = if let Some(file) = self.file.as_mut() {
            file.write(bytes)?
        } else {
            bytes.len()
        };
        self.bytes += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = self.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}
// Standalone benchmark configuration is read at this executable's boundary.
#[allow(clippy::disallowed_methods)]
fn setting(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(name)
}
fn number(name: &str, default: usize) -> usize {
    setting(name).map_or(default, |x| x.parse().unwrap())
}

struct Stimulus {
    width: usize,
    scattered: bool,
    same: bool,
    mask_only: bool,
    notify: bool,
}

impl Stimulus {
    // Keep the timed input updates in a separate function so changes to the
    // collector do not alter register allocation in this per-signal loop.
    #[inline(never)]
    fn apply(
        &self,
        step: usize,
        count: usize,
        descs: &[VcdSignalDesc],
        memory: &mut [u8],
        trace: &TraceLayout,
    ) {
        for j in 0..count {
            let index = if self.scattered {
                (j * descs.len() / count + step) % descs.len()
            } else {
                j
            };
            let home = descs[index].offset;
            let bit = step % self.width.saturating_sub(1).max(1);
            let at = home
                + bit / 8
                + if self.mask_only {
                    self.width.div_ceil(8)
                } else {
                    0
                };
            let next = if self.same {
                memory[at]
            } else {
                memory[at] ^ (1 << (bit % 8))
            };
            memory[at] = black_box(next);
            if self.notify {
                // Same two byte notifications emitted at generated stores.
                unsafe {
                    trace.mark_home(memory.as_mut_ptr(), home);
                }
            }
        }
    }
}

fn main() {
    let signals = number("VCD_SIGNALS", 16_384);
    let steps = number("VCD_STEPS", 1_000);
    let repeats = number("VCD_REPEATS", 3);
    let width = number("VCD_WIDTH", 64);
    let four_state = number("VCD_FOUR_STATE", 0) != 0;
    let mode = setting("VCD_MODE").unwrap_or_else(|_| "dirty".into());
    assert!(["dirty", "scan", "collect"].contains(&mode.as_str()));
    assert!(signals > 0 && width > 0 && repeats > 0);
    let output = setting("VCD_OUTPUT").unwrap_or_else(|_| "count".into());
    let case = setting("VCD_CASE").ok();
    let fixture = Fixture {
        signals,
        width,
        four_state,
    };
    let mut layout = MemoryLayout::build(&fixture, four_state, MemoryLayoutMode::Packed);
    // Same physical layout for every measurement, including full scanning.
    layout.enable_trace();
    let trace = layout.trace.as_ref().unwrap();
    let descs: Vec<_> = (0..signals)
        .map(|i| VcdSignalDesc {
            scope: "top".into(),
            name: format!("s{i}"),
            offset: layout.offsets[&i],
            width,
            is_4state: four_state,
        })
        .collect();
    let size = width.div_ceil(8);
    println!(
        "mode,case,signals,width,four_state,steps,repeat,elapsed_ns,comparisons,changes,bytes"
    );
    for (name, count, scattered, same) in [
        ("idle", 0, false, false),
        ("sparse_clustered", (signals / 1000).max(1), false, false),
        ("sparse_scattered", (signals / 1000).max(1), true, false),
        ("same_value", (signals / 1000).max(1), true, true),
        ("dense", signals, false, false),
        ("burst", signals, false, false),
        ("mask_only", (signals / 1000).max(1), true, false),
    ] {
        if case.as_ref().is_some_and(|case| case != name) || (name == "mask_only" && !four_state) {
            continue;
        }
        let stimulus = Stimulus {
            width,
            scattered,
            same,
            mask_only: name == "mask_only",
            notify: mode != "scan",
        };
        for repeat in 0..repeats {
            let mut memory = vec![0; layout.merged_total_size];
            for (i, desc) in descs.iter().enumerate() {
                memory[desc.offset] = i as u8;
                memory[desc.offset + size - 1] |= 1 << ((width - 1) % 8);
            }
            let sink = Sink {
                file: (output != "count").then(|| std::fs::File::create(&output).unwrap()),
                bytes: 0,
            };
            let mut writer = VcdWriter::from_writer(sink, &descs);
            writer.dump(0, &memory).unwrap();
            writer.flush().unwrap();
            let initial_bytes = writer.get_ref().bytes;
            let initial_stats = writer.statistics();
            let mut activity = Vec::with_capacity(trace.group_count);
            let start = Instant::now();
            for step in 1..=steps {
                let count = if name == "burst" && step % 100 != 0 {
                    0
                } else {
                    count
                };
                if count != 0 {
                    stimulus.apply(step, count, &descs, &mut memory, trace);
                }
                if mode != "scan" {
                    trace.take(&mut memory, &mut activity);
                }
                if mode == "collect" {
                    black_box(&activity);
                } else {
                    writer
                        .dump_with_activity(
                            step as u64,
                            black_box(&memory),
                            &[],
                            (mode == "dirty").then_some(&activity),
                        )
                        .unwrap();
                }
            }
            writer.flush().unwrap();
            let elapsed = start.elapsed().as_nanos();
            let stats = writer.statistics();
            println!(
                "{mode},{name},{signals},{width},{four_state},{steps},{repeat},{elapsed},{},{},{}",
                stats.comparisons - initial_stats.comparisons,
                stats.changes - initial_stats.changes,
                writer.get_ref().bytes - initial_bytes
            );
        }
    }
}
