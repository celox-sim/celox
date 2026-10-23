//! Timed-scheduler overhead benchmark for instruction-count comparisons.
//!
//! Usage: `sched_bench <a|ac|b1|b5> <cycles>`
//!
//! Every variant clocks the same 64 registers `rK <= rK + (seed ^ K)`:
//!
//! - `a`: pure RTL driven by [`celox::Simulator::tick`], one tick per cycle.
//! - `ac`: the same RTL through [`celox::Simulation`] with `add_clock`.
//! - `b1`: the clock comes from `always #5 clk = ~clk` and the registers read
//!   `stim`, which `initial forever @(posedge clk) stim = stim + 3` drives.
//! - `b5`: like `b1` with `stim <= stim + 3`.
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
        _ => panic!("unknown variant {variant}: expected a, ac, b1 or b5"),
    };
    // One time unit per tick, so that `#5` and `add_clock(10, 5)` agree.
    let mut body = String::from("`timescale 1fs/1fs\n");
    body.push_str(&header);
    for lane in 0..LANES {
        body.push_str(&format!("logic [31:0] r{lane} = 0;\n"));
        body.push_str(&format!(
            "always_ff @(posedge clk) r{lane} <= r{lane} + ({seed} ^ 32'd{lane});\n"
        ));
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
    let out = if variant == "a" {
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
        let mut sim = celox::Simulation::from_sv_sources(sources, "Top")
            .build()
            .unwrap();
        if variant == "ac" {
            let seed = sim.signal("seed");
            sim.modify(|io| io.set(seed, 7u32)).unwrap();
            sim.add_clock("clk", 10, 5);
        }
        sim.run_until(cycles * 10).unwrap();
        let out = sim.signal("out");
        sim.get(out)
    };
    println!("{variant} {cycles} out={out:?}");
}
