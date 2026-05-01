//! Benchmark SIRT optimization passes and Cranelift backend options.
//!
//! Measures:
//!   1. Individual pass disable (same as before)
//!   2. Combinations of candidate passes disabled together (interaction effects)
//!   3. Cranelift backend options (regalloc, alias analysis, verifier)
//!
//! Two designs:
//!   - top_n1000: 1000 counters (sequential-heavy, large)
//!   - linear_sec: SEC encoder/decoder (combinational-heavy)

use std::time::{Duration, Instant};

use celox::{
    CraneliftOptLevel, CraneliftOptions, OptimizeOptions, RegallocAlgorithm, Simulator,
    SimulatorBuilder, SirPass,
};

#[path = "../tests/fixtures/veryl_std.rs"]
mod veryl_std;

const TOP_N1000: &str = include_str!("../../../benches/veryl/top_n1000.veryl");

fn linear_sec_source() -> String {
    format!(
        "{}\n{}\n{}",
        veryl_std::source(&["coding", "linear_sec_encoder.veryl"]),
        veryl_std::source(&["coding", "linear_sec_decoder.veryl"]),
        include_str!("../../../benches/veryl/linear_sec_top.veryl"),
    )
}

const WARMUP_ITERS: u32 = 2;
const BENCH_ITERS: u32 = 5;
const TICK_COUNT: u64 = 1_000_000;
const EVAL_COUNT: u64 = 1_000_000;

fn median(values: &mut [Duration]) -> Duration {
    values.sort();
    values[values.len() / 2]
}

// ── Builder configuration ───────────────────────────────────────────

#[derive(Clone)]
#[allow(dead_code)]
struct Config {
    name: String,
    opt: OptimizeOptions,
    cl: CraneliftOptions,
}

impl Config {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            opt: OptimizeOptions::all(),
            cl: CraneliftOptions::default(),
        }
    }

    fn baseline() -> Self {
        Self::new("ALL ENABLED (baseline)")
    }

    fn apply_to<'a>(
        &self,
        builder: SimulatorBuilder<'a, Simulator>,
    ) -> SimulatorBuilder<'a, Simulator> {
        builder
            .optimize_options(self.opt.clone())
            .cranelift_options(self.cl)
    }
}

// ── Measurement helpers ─────────────────────────────────────────────

fn bench_compile(code: &str, top: &str, cfg: &Config) -> Duration {
    for _ in 0..WARMUP_ITERS {
        let _ = cfg.apply_to(Simulator::builder(code, top)).build().unwrap();
    }
    let mut times = Vec::new();
    for _ in 0..BENCH_ITERS {
        let start = Instant::now();
        let _ = cfg.apply_to(Simulator::builder(code, top)).build().unwrap();
        times.push(start.elapsed());
    }
    median(&mut times)
}

fn bench_tick(code: &str, top: &str, cfg: &Config, count: u64) -> Duration {
    let mut sim = cfg.apply_to(Simulator::builder(code, top)).build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Warmup
    for _ in 0..count / 10 {
        sim.tick(clk).unwrap();
    }
    let start = Instant::now();
    for _ in 0..count {
        sim.tick(clk).unwrap();
    }
    start.elapsed()
}

fn bench_eval(code: &str, top: &str, cfg: &Config, count: u64) -> Duration {
    let mut sim = cfg.apply_to(Simulator::builder(code, top)).build().unwrap();
    let i_word = sim.signal("i_word");

    // Warmup
    for i in 0..count / 10 {
        sim.modify(|io| io.set(i_word, i)).unwrap();
        sim.eval_comb().unwrap();
    }
    let start = Instant::now();
    for i in 0..count {
        sim.modify(|io| io.set(i_word, i)).unwrap();
        sim.eval_comb().unwrap();
    }
    start.elapsed()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn delta(cur: Duration, base: Duration) -> f64 {
    (cur.as_secs_f64() - base.as_secs_f64()) / base.as_secs_f64() * 100.0
}

fn print_header() {
    println!(
        "{:<45} {:>11}  {:>7}   {:>11}  {:>7}",
        "", "compile(ms)", "Δ%", "sim(ms)", "Δ%"
    );
    println!("{}", "-".repeat(95));
}

fn print_row(
    name: &str,
    compile: Duration,
    sim: Duration,
    base_compile: Duration,
    base_sim: Duration,
) {
    println!(
        "{:<45} {:>8.2}ms  {:>+6.1}%   {:>8.2}ms  {:>+6.1}%",
        name,
        ms(compile),
        delta(compile, base_compile),
        ms(sim),
        delta(sim, base_sim),
    );
}

fn print_baseline(name: &str, compile: Duration, sim: Duration, sim_label: &str, sim_count: u64) {
    println!(
        "{:<45} compile: {:>8.2}ms   {}×{}: {:>8.2}ms",
        name,
        ms(compile),
        sim_label,
        sim_count,
        ms(sim),
    );
}

// ── Main ────────────────────────────────────────────────────────────

fn main() {
    println!("=== Optimization Benchmark (with interactions & Cranelift) ===\n");

    // ================================================================
    // PART 1: Individual SIRT pass disabling
    // ================================================================
    println!("# Part 1: Individual SIRT pass disabling\n");

    let individual_passes: Vec<(&str, SirPass)> = vec![
        ("store_load_forwarding", SirPass::StoreLoadForwarding),
        ("hoist_common_branch_loads", SirPass::HoistCommonBranchLoads),
        ("bit_extract_peephole", SirPass::BitExtractPeephole),
        ("optimize_blocks", SirPass::OptimizeBlocks),
        ("split_wide_commits", SirPass::SplitWideCommits),
        ("commit_sinking", SirPass::CommitSinking),
        ("inline_commit_forwarding", SirPass::InlineCommitForwarding),
        (
            "eliminate_dead_working_stores",
            SirPass::EliminateDeadWorkingStores,
        ),
        ("reschedule", SirPass::Reschedule),
    ];

    let linear_sec_src = linear_sec_source();
    for (design_name, code, top, sim_label, sim_count, is_seq) in [
        (
            "top_n1000 (sequential)",
            TOP_N1000,
            "Top",
            "tick",
            TICK_COUNT,
            true,
        ),
        (
            "linear_sec_p6 (combinational)",
            &linear_sec_src,
            "Top",
            "eval",
            EVAL_COUNT,
            false,
        ),
    ] {
        println!("## {design_name}\n");

        let baseline = Config::baseline();
        let base_compile = bench_compile(code, top, &baseline);
        let base_sim = if is_seq {
            bench_tick(code, top, &baseline, sim_count)
        } else {
            bench_eval(code, top, &baseline, sim_count)
        };
        print_baseline(
            "ALL ENABLED (baseline)",
            base_compile,
            base_sim,
            sim_label,
            sim_count,
        );

        let mut none_cfg = Config::new("ALL DISABLED");
        none_cfg.opt = OptimizeOptions::none();
        let none_compile = bench_compile(code, top, &none_cfg);
        let none_sim = if is_seq {
            bench_tick(code, top, &none_cfg, sim_count)
        } else {
            bench_eval(code, top, &none_cfg, sim_count)
        };
        print_baseline("ALL DISABLED", none_compile, none_sim, sim_label, sim_count);

        println!();
        print_header();

        for (name, pass) in &individual_passes {
            let mut cfg = Config::new(name);
            cfg.opt = cfg.opt.disable(*pass);
            let compile = bench_compile(code, top, &cfg);
            let sim = if is_seq {
                bench_tick(code, top, &cfg, sim_count)
            } else {
                bench_eval(code, top, &cfg, sim_count)
            };
            print_row(name, compile, sim, base_compile, base_sim);
        }
        println!();
    }

    // ================================================================
    // PART 2: Combinations (interaction effects)
    // ================================================================
    println!("# Part 2: Candidate combinations (interaction effects)\n");

    // Candidates from Part 1: store_load_forwarding, hoist_common_branch_loads, inline_commit_forwarding
    let combinations: Vec<(&str, Vec<SirPass>)> = vec![
        ("−slf", vec![SirPass::StoreLoadForwarding]),
        ("−hcbl", vec![SirPass::HoistCommonBranchLoads]),
        ("−icf", vec![SirPass::InlineCommitForwarding]),
        (
            "−slf −hcbl",
            vec![
                SirPass::StoreLoadForwarding,
                SirPass::HoistCommonBranchLoads,
            ],
        ),
        (
            "−slf −icf",
            vec![
                SirPass::StoreLoadForwarding,
                SirPass::InlineCommitForwarding,
            ],
        ),
        (
            "−hcbl −icf",
            vec![
                SirPass::HoistCommonBranchLoads,
                SirPass::InlineCommitForwarding,
            ],
        ),
        (
            "−slf −hcbl −icf",
            vec![
                SirPass::StoreLoadForwarding,
                SirPass::HoistCommonBranchLoads,
                SirPass::InlineCommitForwarding,
            ],
        ),
    ];

    let linear_sec_src = linear_sec_source();
    for (design_name, code, top, sim_label, sim_count, is_seq) in [
        (
            "top_n1000 (sequential)",
            TOP_N1000,
            "Top",
            "tick",
            TICK_COUNT,
            true,
        ),
        (
            "linear_sec_p6 (combinational)",
            &linear_sec_src,
            "Top",
            "eval",
            EVAL_COUNT,
            false,
        ),
    ] {
        println!("## {design_name}\n");

        let baseline = Config::baseline();
        let base_compile = bench_compile(code, top, &baseline);
        let base_sim = if is_seq {
            bench_tick(code, top, &baseline, sim_count)
        } else {
            bench_eval(code, top, &baseline, sim_count)
        };
        print_baseline(
            "ALL ENABLED (baseline)",
            base_compile,
            base_sim,
            sim_label,
            sim_count,
        );
        println!();
        print_header();

        for (name, passes) in &combinations {
            let mut cfg = Config::new(name);
            for pass in passes {
                cfg.opt = cfg.opt.disable(*pass);
            }
            let compile = bench_compile(code, top, &cfg);
            let sim = if is_seq {
                bench_tick(code, top, &cfg, sim_count)
            } else {
                bench_eval(code, top, &cfg, sim_count)
            };
            print_row(name, compile, sim, base_compile, base_sim);
        }
        println!();
    }

    // ================================================================
    // PART 3: Cranelift backend options
    // ================================================================
    println!("# Part 3: Cranelift backend options\n");

    let cranelift_configs: Vec<(&str, fn(&mut CraneliftOptions))> = vec![
        ("opt_level=None", |c| c.opt_level = CraneliftOptLevel::None),
        ("opt_level=SpeedAndSize", |c| {
            c.opt_level = CraneliftOptLevel::SpeedAndSize
        }),
        ("regalloc=SinglePass", |c| {
            c.regalloc_algorithm = RegallocAlgorithm::SinglePass
        }),
        ("enable_alias_analysis=false", |c| {
            c.enable_alias_analysis = false
        }),
        ("enable_verifier=false", |c| c.enable_verifier = false),
        ("regalloc=SP + alias=false", |c| {
            c.regalloc_algorithm = RegallocAlgorithm::SinglePass;
            c.enable_alias_analysis = false;
        }),
        ("regalloc=SP + alias=false + verifier=false", |c| {
            c.regalloc_algorithm = RegallocAlgorithm::SinglePass;
            c.enable_alias_analysis = false;
            c.enable_verifier = false;
        }),
        ("fast_compile()", |c| *c = CraneliftOptions::fast_compile()),
    ];

    let linear_sec_src = linear_sec_source();
    for (design_name, code, top, sim_label, sim_count, is_seq) in [
        (
            "top_n1000 (sequential)",
            TOP_N1000,
            "Top",
            "tick",
            TICK_COUNT,
            true,
        ),
        (
            "linear_sec_p6 (combinational)",
            &linear_sec_src,
            "Top",
            "eval",
            EVAL_COUNT,
            false,
        ),
    ] {
        println!("## {design_name}\n");

        let baseline = Config::baseline();
        let base_compile = bench_compile(code, top, &baseline);
        let base_sim = if is_seq {
            bench_tick(code, top, &baseline, sim_count)
        } else {
            bench_eval(code, top, &baseline, sim_count)
        };
        print_baseline(
            "ALL ENABLED (baseline)",
            base_compile,
            base_sim,
            sim_label,
            sim_count,
        );
        println!();
        print_header();

        for (name, apply) in &cranelift_configs {
            let mut cfg = Config::new(name);
            apply(&mut cfg.cl);
            let compile = bench_compile(code, top, &cfg);
            let sim = if is_seq {
                bench_tick(code, top, &cfg, sim_count)
            } else {
                bench_eval(code, top, &cfg, sim_count)
            };
            print_row(name, compile, sim, base_compile, base_sim);
        }
        println!();
    }

    // ================================================================
    // PART 4: Combined SIRT + Cranelift (proposed new defaults)
    // ================================================================
    println!("# Part 4: Proposed new defaults\n");

    let disable_three = vec![
        SirPass::StoreLoadForwarding,
        SirPass::HoistCommonBranchLoads,
        SirPass::InlineCommitForwarding,
    ];
    let proposals: Vec<(&str, Box<dyn Fn(&mut Config)>)> = vec![
        ("current defaults (all on)", Box::new(|_: &mut Config| {})),
        ("−slf −hcbl −icf (SIRT only)", {
            let d = disable_three.clone();
            Box::new(move |c: &mut Config| {
                for &p in &d {
                    c.opt = c.opt.clone().disable(p);
                }
            })
        }),
        ("−slf −hcbl −icf + verifier=false", {
            let d = disable_three.clone();
            Box::new(move |c: &mut Config| {
                for &p in &d {
                    c.opt = c.opt.clone().disable(p);
                }
                c.cl.enable_verifier = false;
            })
        }),
        ("−slf −hcbl −icf + alias=false + verifier=false", {
            let d = disable_three.clone();
            Box::new(move |c: &mut Config| {
                for &p in &d {
                    c.opt = c.opt.clone().disable(p);
                }
                c.cl.enable_alias_analysis = false;
                c.cl.enable_verifier = false;
            })
        }),
    ];

    for (design_name, code, top, _sim_label, sim_count, is_seq) in [
        (
            "top_n1000 (sequential)",
            TOP_N1000,
            "Top",
            "tick",
            TICK_COUNT,
            true,
        ),
        (
            "linear_sec_p6 (combinational)",
            &linear_sec_src,
            "Top",
            "eval",
            EVAL_COUNT,
            false,
        ),
    ] {
        println!("## {design_name}\n");

        let baseline = Config::baseline();
        let base_compile = bench_compile(code, top, &baseline);
        let base_sim = if is_seq {
            bench_tick(code, top, &baseline, sim_count)
        } else {
            bench_eval(code, top, &baseline, sim_count)
        };
        println!();
        print_header();

        for (name, apply) in &proposals {
            let mut cfg = Config::new(name);
            apply(&mut cfg);
            let compile = bench_compile(code, top, &cfg);
            let sim = if is_seq {
                bench_tick(code, top, &cfg, sim_count)
            } else {
                bench_eval(code, top, &cfg, sim_count)
            };
            print_row(name, compile, sim, base_compile, base_sim);
        }
        println!();
    }
}
