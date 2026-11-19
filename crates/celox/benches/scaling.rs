//! Scheduler scaling and whole-design fallback benchmarks.
//!
//! Each design is a generated SystemVerilog design with its own clock
//! generator, built like the `celox` command builds an RTLMeter testbench
//! (native backend, O2, two-state, one thread) and run by the timed
//! simulation.
//!
//! - `scaling/<shape>/{compile,sim}/n<N>`: the same shape at two sizes, so
//!   the CodSpeed history shows the slope in N. `clock_alias` gives every
//!   flop its own copy of the clock (VeeR's `assign l1clk = clk`) while the
//!   testbench senses the clock at the other edge; `gated_clock` gives every
//!   flop a clock gate (`en_ff` captured at negedge, `gclk = clk & en_ff`).
//! - `fallback/<construct>/sim`: a fixed design of 64 combinational lanes
//!   with ONE extra construct, next to `fallback/baseline/sim`. A construct
//!   that makes the whole design take a slower path (no settle skipping, no
//!   partitioned settle, one compiled run per slot, the reference walk)
//!   shows as a gap to the baseline; the pair should stay close.
//!
//! Simulation benchmarks report the time of [`WINDOW`] clock cycles.

// CodSpeed's Criterion harness reads its own environment during macro expansion.
#![allow(clippy::disallowed_methods)]

use std::path::Path;

use celox::{DefaultBackend, OptLevel, Simulation, SimulatorBuilder};
use codspeed_criterion_compat::{
    Criterion, IterManualOptions, black_box, criterion_group, criterion_main,
};

/// Clock cycles per simulation sample.
const WINDOW: u64 = 1_000;
/// Simulated time of one clock cycle (`always #5 clk = ~clk` at 1ns/1ns).
const PERIOD: u64 = 10;
/// The two sizes of each scaling shape.
const SIZES: [usize; 2] = [16, 1024];

const PRELUDE: &str = "`timescale 1ns/1ns\n";

/// Clock and a cycle counter sensed at negedge, as VeeR's `tb_top` senses
/// `core_clk`: a copy of `clk` sensed at posedge is then a domain of its own.
const CLOCK: &str = "
  bit clk = 0;
  always #5 clk = ~clk;
  logic [31:0] cycle = 0;
  always @(negedge clk) cycle <= cycle + 1;
";

/// `n` flops in a chain, each clocked by its own `assign l1clk = clk`.
fn clock_alias(n: usize) -> String {
    format!(
        "{PRELUDE}
module AliasFlop(input logic clk, input logic [31:0] d, output logic [31:0] q);
  logic l1clk;
  assign l1clk = clk;
  always_ff @(posedge l1clk) q <= d;
endmodule

module Top(output logic [31:0] obs);
{CLOCK}
  logic [31:0] chain [0:{n}];
  assign chain[0] = cycle;
  for (genvar i = 0; i < {n}; i++) begin : g
    AliasFlop flop(.clk(clk), .d(chain[i] + 32'(i)), .q(chain[i + 1]));
  end
  assign obs = chain[{n}];
endmodule
"
    )
}

/// `n` flops in a chain, each behind a clock gate whose enable is captured
/// at negedge; flop `i` skips one cycle in 16.
fn gated_clock(n: usize) -> String {
    format!(
        "{PRELUDE}
module Icg(input logic clk, input logic en, output logic gclk);
  logic en_ff;
  always @(negedge clk) en_ff <= en;
  assign gclk = clk & en_ff;
endmodule

module GatedFlop(input logic clk, input logic en, input logic [31:0] d, output logic [31:0] q);
  logic gclk;
  Icg icg(.clk(clk), .en(en), .gclk(gclk));
  always_ff @(posedge gclk) q <= d;
endmodule

module Top(output logic [31:0] obs);
{CLOCK}
  logic [31:0] chain [0:{n}];
  assign chain[0] = cycle;
  for (genvar i = 0; i < {n}; i++) begin : g
    GatedFlop flop(.clk(clk), .en(cycle[3:0] != 4'(i)), .d(chain[i] + 32'(i)), .q(chain[i + 1]));
  end
  assign obs = chain[{n}];
endmodule
"
    )
}

/// 64 combinational lanes between flops, plus `extra` (one construct).
/// `verbose` is a top input nothing drives, so code under it never runs
/// but cannot be folded away.
fn lanes(extra: &str) -> String {
    format!(
        "{PRELUDE}
module Top(input logic verbose, output logic [31:0] obs);
  bit clk = 0;
  always #5 clk = ~clk;
  logic [31:0] r [64];
  logic [31:0] c [64];
  for (genvar i = 0; i < 64; i++) begin : lane
    assign c[i] = (r[i] ^ (r[(i + 1) % 64] >> 3)) + 32'h9E3779B9 * 32'(i + 1);
    always_ff @(posedge clk) r[i] <= c[i] + r[(i + 63) % 64];
  end
  logic [31:0] acc;
  always_comb begin
    acc = 0;
    for (int i = 0; i < 64; i++) acc ^= c[i];
  end
  logic [31:0] extra = 0;
  assign obs = acc ^ extra;
{extra}
endmodule
"
    )
}

/// The constructs of the fallback benchmarks, each added alone to [`lanes`].
const FALLBACKS: &[(&str, &str)] = &[
    ("baseline", ""),
    (
        "comb_display",
        "  always_comb if (verbose) $display(\"acc %h\", acc);",
    ),
    (
        "deferred_assert",
        "  always_comb assert final (!verbose || acc != 0);",
    ),
    (
        "dpi_always_star",
        "  import \"DPI-C\" function int unsigned celox_bench_mix(input int unsigned x);
  always @* extra = celox_bench_mix(acc);",
    ),
    (
        "string_array",
        "  string names [4];
  initial foreach (names[i]) names[i] = $sformatf(\"lane%0d\", i);
  always_ff @(posedge clk) extra <= 32'(names[r[0][1:0]].len());",
    ),
    (
        "final_block",
        "  final if (verbose) $display(\"obs %h\", obs);",
    ),
];

/// The DPI-C function of `dpi_always_star`: a cheap pure mix.
extern "C" fn celox_bench_mix(x: u32) -> u32 {
    x.rotate_left(7) ^ 0x5bd1_e995
}

fn builder(source: &str) -> SimulatorBuilder<'_, Simulation> {
    let builder = Simulation::from_sv_sources(vec![(source, Path::new("bench.sv"))], "Top")
        .opt_level(OptLevel::O2)
        .four_state(false)
        .threads(1);
    // SAFETY: `celox_bench_mix` has the signature of its import
    // (`int unsigned` in, `int unsigned` out) and lives for the program.
    unsafe { builder.dpi_function("celox_bench_mix", celox_bench_mix as *const ()) }
}

fn build(source: &str) -> Simulation<DefaultBackend> {
    builder(source)
        .build_native()
        .expect("benchmark design should build")
}

fn run_window(sim: &mut Simulation<DefaultBackend>) {
    let end = sim.time() + WINDOW * PERIOD;
    sim.run_until(end).expect("benchmark design should run");
}

/// Build `source`, run one window to leave start-up behind, print how the
/// scheduler runs it (which explains a gap), then measure windows.
fn bench_simulation(c: &mut Criterion, name: &str, source: &str) {
    // Built inside the benchmark, so a filtered-out benchmark builds nothing.
    c.bench_function(name, |b| {
        let mut sim = build(source);
        run_window(&mut sim);
        let stats = sim.scheduler_stats();
        let reference = stats
            .kernels
            .iter()
            .filter(|kernel| !matches!(kernel, celox::KernelSchedule::Compiled))
            .count();
        println!(
            "{name}: compiled_slots={} compiled_runs={} reference_steps={} reference_kernels={reference}",
            stats.compiled_slots, stats.compiled_runs, stats.reference_steps,
        );
        b.iter_manual_unstable(IterManualOptions::new().rounds(10).warmup(1), || {
            run_window(&mut sim)
        });
    });
}

/// Compile `source` `rounds` times (at least 2, which the walltime
/// analysis needs; the large sizes take seconds each).
fn bench_compile(c: &mut Criterion, name: &str, source: &str, rounds: u64) {
    c.bench_function(name, |b| {
        b.iter_manual_unstable(IterManualOptions::new().rounds(rounds), || {
            black_box(build(black_box(source)))
        });
    });
}

fn benchmark_scaling(c: &mut Criterion) {
    // Warm process-global frontend state outside the measured spans.
    drop(build(&clock_alias(1)));
    for (shape, generate) in [
        ("clock_alias", clock_alias as fn(usize) -> String),
        ("gated_clock", gated_clock),
    ] {
        for n in SIZES {
            let source = generate(n);
            let rounds = if n <= 64 { 5 } else { 2 };
            bench_compile(c, &format!("scaling/{shape}/compile/n{n}"), &source, rounds);
            bench_simulation(c, &format!("scaling/{shape}/sim/n{n}"), &source);
        }
    }
}

fn benchmark_fallbacks(c: &mut Criterion) {
    for (construct, extra) in FALLBACKS {
        bench_simulation(c, &format!("fallback/{construct}/sim"), &lanes(extra));
    }
}

criterion_group!(benches, benchmark_scaling, benchmark_fallbacks);
criterion_main!(benches);
