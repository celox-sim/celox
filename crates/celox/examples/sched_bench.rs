//! Timed-scheduler overhead benchmark for instruction-count comparisons.
//!
//! Usage: `sched_bench <a|ac|b1|b5|bh|d|dr> <cycles> [default|reference|compiled] [native|interp]`
//!
//! Every variant clocks the same 64 registers `rK <= rK + (seed ^ K)`:
//!
//! - `a`: pure RTL driven by [`celox::Simulator::tick`], one tick per cycle.
//! - `ac`: the same RTL through [`celox::Simulation`] with `add_clock`.
//! - `b1`: the clock comes from `always #5 clk = ~clk` and the registers read
//!   `stim`, which `initial forever @(posedge clk) stim = stim + 3` drives.
//! - `b5`: like `b1` with `stim <= stim + 3`.
//! - `bh`: like `b1` with the clock from `add_clock` instead of a kernel.
//! - `d`: the clock comes from `always #5 clk = ~clk` and the registers read
//!   a constant seed, so only delay-only kernels run.
//! - `dr`: like `d` with an asynchronous reset input that the host sets
//!   once and no clock drives.
//!
//! The optional third argument selects the timed scheduler (hidden builder
//! option), by default the library's; the fourth the backend, by default the
//! host's native one.
//!
//! The scheduler's cost per cycle is the difference of the whole-program
//! instruction counts of two runs, which cancels compilation:
//!
//! ```sh
//! cargo build --release -p celox --features systemverilog --example sched_bench
//! valgrind --tool=callgrind target/release/examples/sched_bench b1 200000
//! valgrind --tool=callgrind target/release/examples/sched_bench b1 20000
//! ```
//!
//! (Ir at 200,000 - Ir at 20,000) / 180,000, median of five runs each.

const LANES: usize = 64;

fn source(variant: &str) -> String {
    let (header, seed) = match variant {
        "a" | "ac" => (
            "module Top(input logic clk, input logic [31:0] seed, output logic [31:0] out);\n"
                .to_string(),
            "seed",
        ),
        "b1" | "b5" => {
            let assign = if variant == "b1" { "=" } else { "<=" };
            (
                format!(
                    "module Top(output logic [31:0] out);\n\
                     logic clk = 0;\n\
                     logic [31:0] stim = 0;\n\
                     always #5 clk = ~clk;\n\
                     initial forever @(posedge clk) stim {assign} stim + 3;\n"
                ),
                "stim",
            )
        }
        "bh" => (
            "module Top(input logic clk, output logic [31:0] out);\n\
             logic [31:0] stim = 0;\n\
             initial forever @(posedge clk) stim = stim + 3;\n"
                .to_string(),
            "stim",
        ),
        "d" | "dr" => {
            let ports = if variant == "d" {
                "output logic [31:0] out"
            } else {
                "input logic rst_n, output logic [31:0] out"
            };
            (
                format!(
                    "module Top({ports});\n\
                     logic clk = 0;\n\
                     logic [31:0] seed = 7;\n\
                     always #5 clk = ~clk;\n"
                ),
                "seed",
            )
        }
        _ => panic!("unknown variant {variant}: expected a, ac, b1, b5, bh, d or dr"),
    };
    // One time unit per tick, so that `#5` and `add_clock(10, 5)` agree.
    let mut body = String::from("`timescale 1fs/1fs\n");
    body.push_str(&header);
    for lane in 0..LANES {
        body.push_str(&format!("logic [31:0] r{lane} = 0;\n"));
        if variant == "dr" {
            body.push_str(&format!(
                "always_ff @(posedge clk or negedge rst_n) \
                 if (!rst_n) r{lane} <= 0; else r{lane} <= r{lane} + ({seed} ^ 32'd{lane});\n"
            ));
        } else {
            body.push_str(&format!(
                "always_ff @(posedge clk) r{lane} <= r{lane} + ({seed} ^ 32'd{lane});\n"
            ));
        }
    }
    let lanes: Vec<String> = (0..LANES).map(|lane| format!("r{lane}")).collect();
    body.push_str(&format!("assign out = {};\nendmodule\n", lanes.join(" + ")));
    body
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let variant = args.get(1).map(String::as_str).unwrap_or("a");
    let cycles: u64 = args
        .get(2)
        .map(|cycles| cycles.parse().expect("cycles must be a number"))
        .unwrap_or(20_000);
    let source = source(variant);
    let sources = vec![(source.as_str(), std::path::Path::new("sched_bench.sv"))];
    let interpreter = match args.get(4).map(String::as_str) {
        None | Some("native") => false,
        Some("interp") => true,
        Some(other) => panic!("unknown backend {other}: expected native or interp"),
    };
    let out = if variant == "a" && interpreter {
        let sim = celox::Simulator::from_sv_sources(sources, "Top")
            .build_interpreter()
            .unwrap();
        tick(sim, cycles)
    } else if variant == "a" {
        // The loop of the native case stays in `main`, as the baseline has it.
        let mut sim = celox::Simulator::from_sv_sources(sources, "Top")
            .build()
            .unwrap();
        let seed = sim.signal("seed");
        sim.modify(|io| io.set(seed, 7u32)).unwrap();
        let clk = sim.event("clk");
        for _ in 0..cycles {
            sim.tick(clk).unwrap();
        }
        let out = sim.signal("out");
        sim.get(out)
    } else {
        let mode = match args.get(3).map(String::as_str) {
            None | Some("default") => celox::SchedulerMode::default(),
            Some("reference") => celox::SchedulerMode::Reference,
            Some("compiled") => celox::SchedulerMode::Compiled,
            Some(other) => panic!("unknown scheduler {other}: expected reference or compiled"),
        };
        let builder = celox::Simulation::from_sv_sources(sources, "Top").scheduler_mode(mode);
        if interpreter {
            run(builder.build_interpreter().unwrap(), variant, cycles)
        } else {
            run(builder.build().unwrap(), variant, cycles)
        }
    };
    println!("{variant} {cycles} out={out:?}");
}

/// Variant `a` on another backend: one tick per cycle.
fn tick<B: celox::SimBackend>(mut sim: celox::Simulator<B>, cycles: u64) -> celox::BigUint {
    let seed = sim.signal("seed");
    sim.modify(|io| io.set(seed, 7u32)).unwrap();
    let clk = sim.event("clk");
    for _ in 0..cycles {
        sim.tick(clk).unwrap();
    }
    let out = sim.signal("out");
    sim.get(out)
}

/// The timed variants: run `cycles` clock periods.
fn run<B: celox::SimBackend>(
    mut sim: celox::Simulation<B>,
    variant: &str,
    cycles: u64,
) -> celox::BigUint {
    if variant == "ac" {
        let seed = sim.signal("seed");
        sim.modify(|io| io.set(seed, 7u32)).unwrap();
    }
    if variant == "dr" {
        let rst_n = sim.signal("rst_n");
        sim.modify(|io| io.set(rst_n, 1u8)).unwrap();
    }
    if variant == "ac" || variant == "bh" {
        sim.add_clock("clk", 10, 5);
    }
    sim.run_until(cycles * 10).unwrap();
    let stats = sim.scheduler_stats();
    println!(
        "compiled slots {}, kernels {:?}",
        stats.compiled_slots, stats.kernels
    );
    let out = sim.signal("out");
    sim.get(out)
}
