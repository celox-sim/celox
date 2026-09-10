//! End-to-end cost of compiled write notifications and waveform output.
use celox::{SimBackend, Simulator, SimulatorBuilder};
use std::{fmt::Write, time::Instant};

// Standalone benchmark configuration is read at this executable's boundary.
#[allow(clippy::disallowed_methods)]
fn setting(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(name)
}
fn number(name: &str, default: usize) -> usize {
    setting(name).map_or(default, |x| x.parse().unwrap())
}
fn run<B: SimBackend>(
    mut sim: Simulator<B>,
    mode: &str,
    case: &str,
    active: usize,
    signals: usize,
    steps: usize,
    repeat: usize,
) {
    let clk = sim.event("clk");
    let clock_signal = sim.signal("clk");
    sim.set(sim.signal("rst"), 0u8);
    sim.tick(clk).unwrap();
    sim.set(sim.signal("rst"), 1u8);
    for i in 0..signals {
        sim.set(sim.signal(&format!("en{i}")), u8::from(i < active));
    }
    sim.tick(clk).unwrap();
    if mode == "scan" {
        // Force the safe reference path while keeping identical generated code.
        let _ = sim.memory_as_mut_ptr();
    }
    if mode == "dirty" || mode == "scan" {
        sim.dump(0);
        sim.flush_vcd().unwrap();
    }
    let initial = sim.vcd_statistics().unwrap_or_default();
    let start = Instant::now();
    for step in 1..=steps {
        sim.set(clock_signal, 0u8);
        sim.eval_comb().unwrap();
        if mode == "dirty" || mode == "scan" {
            sim.dump((step * 2) as u64);
        }
        sim.set(clock_signal, 1u8);
        sim.tick(clk).unwrap();
        if mode == "dirty" || mode == "scan" {
            sim.dump((step * 2 + 1) as u64);
        }
    }
    sim.flush_vcd().unwrap();
    let elapsed = start.elapsed().as_nanos();
    let stats = sim.vcd_statistics().unwrap_or_default();
    std::hint::black_box(sim.get_as::<u64>(sim.signal("q0")));
    println!(
        "{mode},{case},{signals},{steps},{repeat},{elapsed},{},{},{}",
        stats.comparisons - initial.comparisons,
        stats.changes - initial.changes,
        stats.value_bytes - initial.value_bytes
    );
}
fn main() {
    let signals = number("VCD_SIGNALS", 256);
    let steps = number("VCD_STEPS", 10_000);
    let repeats = number("VCD_REPEATS", 3);
    // Keep all 64 value bits significant when comparing encoders that do not
    // abbreviate leading zeros (for example Verilator's VCD writer).
    let high_bit = if number("VCD_FULL_WIDTH", 0) == 0 {
        0
    } else {
        1u64 << 63
    };
    let backend = setting("VCD_BACKEND").unwrap_or_else(|_| "native".into());
    let output = setting("VCD_OUTPUT").unwrap_or_else(|_| "/dev/null".into());
    let selected_mode = setting("VCD_MODE").ok();
    let selected_case = setting("VCD_CASE").ok();
    let mut code = String::from("module Top (clk: input clock, rst: input reset,");
    for i in 0..signals {
        write!(code, "en{i}: input logic, q{i}: output logic<64>,").unwrap();
    }
    code.push_str(") {");
    for i in 0..signals {
        write!(code, "always_ff (clk, rst) {{ if_reset {{ q{i} = 64'd{}; }} else if en{i} {{ q{i} += 64'd{}; }} }}", high_bit | (i as u64 + 1), i * 2 + 1).unwrap();
    }
    code.push('}');
    println!("mode,case,signals,steps,repeat,elapsed_ns,comparisons,changes,value_bytes");
    for (case, active) in [
        ("idle", 0),
        ("sparse", (signals / 100).max(1)),
        ("dense", signals),
    ] {
        if selected_case.as_ref().is_some_and(|name| name != case) {
            continue;
        }
        for mode in ["off", "instrumented", "scan", "dirty"] {
            if selected_mode.as_ref().is_some_and(|name| name != mode) {
                continue;
            }
            for repeat in 0..repeats {
                let builder = SimulatorBuilder::new(&code, "Top");
                let builder = if mode == "off" {
                    builder
                } else {
                    builder.vcd(&output)
                };
                match backend.as_str() {
                    "native" => run(
                        builder.build().unwrap(),
                        mode,
                        case,
                        active,
                        signals,
                        steps,
                        repeat,
                    ),
                    "cranelift" => run(
                        builder.build_cranelift().unwrap(),
                        mode,
                        case,
                        active,
                        signals,
                        steps,
                        repeat,
                    ),
                    "interpreter" => run(
                        builder.build_interpreter().unwrap(),
                        mode,
                        case,
                        active,
                        signals,
                        steps,
                        repeat,
                    ),
                    other => panic!("unknown VCD_BACKEND: {other}"),
                }
            }
        }
    }
}
