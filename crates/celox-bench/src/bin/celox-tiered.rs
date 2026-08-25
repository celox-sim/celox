#![allow(clippy::disallowed_macros)] // CLI errors intentionally use stderr

use std::time::{Duration, Instant};

use celox::{SimulatorBuilder, TierPromotion};
use clap::{Parser, ValueEnum};

const LCG_SOURCE: &str = r#"
module TieredBenchTop (
    clk: input clock,
    d: input logic<64>,
    q: output logic<64>,
) {
    var acc: logic<64>;
    always_ff (clk) {
        acc = acc * 6364136223846793005 + d + 1442695040888963407;
    }
    assign q = acc;
}
"#;

const SORTER_TOP: &str = "SorterTreeDistEntry";

fn sorter_source() -> &'static str {
    const FIXTURES: &[&str] = &[
        "sorter_item.veryl",
        "dist_entry.veryl",
        "min_reduction_tree.veryl",
        "linear_sorter_pull.veryl",
        "linear_sorter.veryl",
        "sorter_tree.veryl",
    ];
    let mut joined = String::new();
    for file in FIXTURES {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../celox/tests/fixtures/sorter_tree/"
        )
        .to_owned()
            + file;
        joined.push_str(&std::fs::read_to_string(path).expect("sorter fixture"));
        joined.push('\n');
    }
    Box::leak(joined.into_boxed_str())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Workload {
    Lcg,
    Sorter { n: u64 },
}

impl Workload {
    fn as_str(self) -> String {
        match self {
            Self::Lcg => "lcg".to_owned(),
            Self::Sorter { n } => format!("sorter_n{n}"),
        }
    }
}

#[derive(Parser)]
#[command(about = "Compare tiered startup/throughput against the compiled and interpreted tiers")]
struct Cli {
    #[arg(long)]
    ticks: Option<u64>,
    #[arg(long, value_enum, default_value = "tiered")]
    mode: Mode,
    #[arg(long, value_enum, default_value = "always")]
    promotion: Promotion,
    /// Minimum interpreted steps before adoption when --promotion after-steps.
    #[arg(long, default_value_t = 0)]
    threshold: u64,
    /// Ticks per timing sample; promotion is observed once per chunk.
    #[arg(long, default_value_t = 1_000)]
    chunk: u32,
    /// Run the SorterTreeDistEntry fixture at the given N instead of the
    /// tiny LCG module.
    #[arg(long)]
    sorter_n: Option<u64>,
}

impl Cli {
    fn workload(&self) -> Workload {
        match self.sorter_n {
            Some(n) => Workload::Sorter { n },
            None => Workload::Lcg,
        }
    }

    fn tick_count(&self) -> u64 {
        self.ticks.unwrap_or(match self.workload() {
            Workload::Lcg => 100_000,
            Workload::Sorter { .. } => 2_000,
        })
    }

    fn source(&self) -> &'static str {
        match self.workload() {
            Workload::Lcg => LCG_SOURCE,
            Workload::Sorter { .. } => sorter_source(),
        }
    }

    fn builder(&self) -> SimulatorBuilder<'static> {
        let mut builder = Simulator::builder(self.source(), self.top())
            // Explicit active-high reset: the sorter fixture's plain `reset`
            // port would otherwise default to AsyncLow and invert the
            // assert/deassert sequence below.
            .reset_type(celox::ResetType::AsyncHigh);
        if let Workload::Sorter { n } = self.workload() {
            builder = builder
                .param("N", n)
                .param("LEAF_DEPTH", 4)
                .param("OUT_DEPTH", 16);
        }
        builder
    }

    fn top(&self) -> &'static str {
        match self.workload() {
            Workload::Lcg => "TieredBenchTop",
            Workload::Sorter { .. } => SORTER_TOP,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Mode {
    Native,
    Cranelift,
    Interpreter,
    Tiered,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Cranelift => "cranelift",
            Self::Interpreter => "interpreter",
            Self::Tiered => "tiered",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Promotion {
    Always,
    AfterSteps,
    Never,
}

impl Promotion {
    fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::AfterSteps => "after-steps",
            Self::Never => "never",
        }
    }
}

enum BenchSim {
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    Native(Box<celox::Simulator<celox::NativeBackend>>),
    Cranelift(Box<celox::Simulator<celox::JitBackend>>),
    Interpreter(Box<celox::Simulator<celox::InterpBackend>>),
    Tiered(Box<celox::Simulator<celox::TieredBackend>>),
}

#[derive(Debug, thiserror::Error)]
enum BenchError {
    #[error("Celox build failed: {0:?}")]
    Build(#[from] celox::SimulatorError),
    #[error("{message}")]
    InvalidConfiguration { message: &'static str },
    #[error("runtime error during execution: {0:?}")]
    Runtime(#[from] celox::RuntimeErrorCode),
}

use celox::RuntimeErrorCode;

impl BenchSim {
    fn build(mode: Mode, cli: &Cli) -> Result<(Self, Duration), BenchError> {
        let build_start = Instant::now();
        let sim = match mode {
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            Mode::Native => {
                let mut sim = cli.builder().build_native()?;
                setup(&mut sim, cli)?;
                BenchSim::Native(Box::new(sim))
            }
            #[cfg(not(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            )))]
            Mode::Native => unreachable!("native availability checked in run()"),
            Mode::Cranelift => {
                let mut sim = cli.builder().build_cranelift()?;
                setup(&mut sim, cli)?;
                BenchSim::Cranelift(Box::new(sim))
            }
            Mode::Interpreter => {
                let mut sim = cli.builder().build_interpreter()?;
                setup(&mut sim, cli)?;
                BenchSim::Interpreter(Box::new(sim))
            }
            Mode::Tiered => {
                let policy = match cli.promotion {
                    Promotion::Always => TierPromotion::Always,
                    Promotion::AfterSteps => TierPromotion::AfterSteps(cli.threshold),
                    Promotion::Never => TierPromotion::Never,
                };
                let mut sim = cli.builder().tier_promotion(policy).build_tiered()?;
                setup(&mut sim, cli)?;
                BenchSim::Tiered(Box::new(sim))
            }
        };
        Ok((sim, build_start.elapsed()))
    }

    fn clk_event_id(&self) -> usize {
        match self {
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            BenchSim::Native(sim) => clk_id(sim),
            BenchSim::Cranelift(sim) => clk_id(sim),
            BenchSim::Interpreter(sim) => clk_id(sim),
            BenchSim::Tiered(sim) => clk_id(sim),
        }
    }

    fn tick_chunk(&mut self, event_id: usize, chunk: u32) -> Result<(), RuntimeErrorCode> {
        match self {
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            BenchSim::Native(sim) => sim.tick_by_id_n(event_id, chunk),
            BenchSim::Cranelift(sim) => sim.tick_by_id_n(event_id, chunk),
            BenchSim::Interpreter(sim) => sim.tick_by_id_n(event_id, chunk),
            BenchSim::Tiered(sim) => sim.tick_by_id_n(event_id, chunk),
        }
    }

    fn is_compiled(&self) -> bool {
        match self {
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            BenchSim::Native(_) => true,
            BenchSim::Cranelift(_) => true,
            // The interpreter has no compiled tier; report it as such so the
            // machine-readable records classify the baseline correctly.
            BenchSim::Interpreter(_) => false,
            BenchSim::Tiered(sim) => sim.is_compiled(),
        }
    }

    fn output_hex(&mut self, workload: Workload) -> String {
        match self {
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            BenchSim::Native(sim) => output_hex(sim, workload),
            BenchSim::Cranelift(sim) => output_hex(sim, workload),
            BenchSim::Interpreter(sim) => output_hex(sim, workload),
            BenchSim::Tiered(sim) => output_hex(sim, workload),
        }
    }
}

fn clk_id<B: celox::SimBackend>(sim: &celox::Simulator<B>) -> usize {
    sim.named_events()
        .iter()
        .find(|e| e.name == "clk")
        .expect("clk event")
        .id
}

fn setup<B: celox::SimBackend>(
    sim: &mut celox::Simulator<B>,
    cli: &Cli,
) -> Result<(), RuntimeErrorCode> {
    match cli.workload() {
        Workload::Lcg => {
            let d = sim.signal("d");
            sim.modify(|io| io.set(d, 0x9E37_79B9_7F4A_7C15u64))?;
        }
        Workload::Sorter { n } => {
            // Drive non-trivial stimulus so the timed ticks exercise real
            // sort/merge paths instead of an idle pipeline: continuous
            // pushes of distinct per-lane data.
            let d_in = sim.signal("d_in");
            let mut pattern = Vec::with_capacity(8 * n as usize);
            for lane in 0..n {
                let value = 0x0100_0000_0000_0001u64.wrapping_mul(lane + 1);
                pattern.extend_from_slice(&value.to_le_bytes());
            }
            // `push` is an N-wide per-lane vector: assert every lane so
            // each distinct d_in value is actually inserted.
            let all_lanes = (celox::BigUint::from(1u8) << n as usize) - 1u8;
            let push = sim.signal("push");
            let last = sim.signal("last");
            let merge_en = sim.signal("merge_en");
            sim.modify(|io| {
                io.set_wide(d_in, celox::BigUint::from_bytes_le(&pattern));
                io.set_wide(push, all_lanes);
                io.set(last, 1u8);
                io.set(merge_en, 1u8);
            })?;
            let rst = sim.signal("rst");
            sim.modify(|io| io.set(rst, 1u8))?;
            let clk = clk_id(sim);
            sim.tick_by_id(clk)?;
            sim.modify(|io| io.set(rst, 0u8))?;
        }
    }
    Ok(())
}

fn output_hex<B: celox::SimBackend>(sim: &mut celox::Simulator<B>, workload: Workload) -> String {
    match workload {
        Workload::Lcg => {
            let q = sim.signal("q");
            format!("{:#018x}", sim.get_as::<u64>(q))
        }
        // The sorter has no single wide output; hash the whole sorted-lane
        // vector plus `done` so modes can be cross-checked.
        Workload::Sorter { .. } => {
            let d_out = sim.signal("d_out");
            let done = sim.signal("done");
            let lanes = sim.get(d_out);
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for (index, limb) in lanes.iter_u64_digits().enumerate().take(16) {
                hash = hash.rotate_left(11) ^ hash.wrapping_add(limb.rotate_left(index as u32 % 7));
            }
            hash ^= u64::from(sim.get_as::<u8>(done));
            format!("{hash:#018x}")
        }
    }
}

type Simulator<B> = celox::Simulator<B>;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), BenchError> {
    let cli = Cli::parse();
    if matches!(cli.mode, Mode::Native) && !native_available() {
        return Err(BenchError::InvalidConfiguration {
            message: "the native backend is unavailable on this host or target",
        });
    }
    if cli.chunk == 0 {
        return Err(BenchError::InvalidConfiguration {
            message: "--chunk must be at least 1",
        });
    }
    if cli.tick_count() == 0 {
        return Err(BenchError::InvalidConfiguration {
            message: "--ticks must be at least 1",
        });
    }
    let workload = cli.workload();

    println!(
        "CELOX_TIERED_CONFIG workload={} mode={} ticks={} promotion={} threshold={} chunk={}",
        workload.as_str(),
        cli.mode.as_str(),
        cli.tick_count(),
        cli.promotion.as_str(),
        cli.threshold,
        cli.chunk
    );

    let (mut sim, build_elapsed) = BenchSim::build(cli.mode, &cli)?;
    println!(
        "CELOX_TIERED_BUILD workload={} mode={} build_ns={}",
        workload.as_str(),
        cli.mode.as_str(),
        build_elapsed.as_nanos()
    );

    let clk = sim.clk_event_id();
    let ticks = cli.tick_count();

    let first_tick_start = Instant::now();
    sim.tick_chunk(clk, 1)?;
    let first_tick_elapsed = first_tick_start.elapsed();
    // The first tick is measured separately; remember whether it already
    // promoted so the promotion record matches this observation.
    let compiled_after_first_tick = sim.is_compiled();
    println!(
        "CELOX_TIERED_FIRST_TICK workload={} mode={} first_tick_ns={} compiled_after_first_tick={compiled_after_first_tick}",
        workload.as_str(),
        cli.mode.as_str(),
        first_tick_elapsed.as_nanos()
    );

    let run_start = Instant::now();
    let mut executed: u64 = 1;
    let mut promoted_at: Option<u64> = compiled_after_first_tick.then_some(1);
    while executed < ticks {
        let chunk = u32::try_from(ticks - executed)
            .unwrap_or(u32::MAX)
            .min(cli.chunk);
        sim.tick_chunk(clk, chunk)?;
        executed += u64::from(chunk);
        if promoted_at.is_none() && sim.is_compiled() {
            promoted_at = Some(executed);
        }
    }
    let run_elapsed = run_start.elapsed();

    if let Some(tick) = promoted_at {
        println!(
            "CELOX_TIERED_PROMOTED workload={} mode={} tick={tick}",
            workload.as_str(),
            cli.mode.as_str()
        );
    } else {
        println!(
            "CELOX_TIERED_PROMOTED workload={} mode={} tick=-1",
            workload.as_str(),
            cli.mode.as_str()
        );
    }
    let timed_ticks = executed - 1;
    println!(
        "CELOX_TIERED_RUN workload={} mode={} ticks={timed_ticks} total_ns={} ns_per_tick={} promoted={} output={}",
        workload.as_str(),
        cli.mode.as_str(),
        run_elapsed.as_nanos(),
        run_elapsed.as_nanos() / u128::from(timed_ticks.max(1)),
        sim.is_compiled(),
        sim.output_hex(workload)
    );
    Ok(())
}

fn native_available() -> bool {
    cfg!(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))
}
