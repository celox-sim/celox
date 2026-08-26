//! Statistical benchmarks for tiered execution.
//!
//! Lifecycle position is read from `TieredExecutionStats`; elapsed time,
//! sampling, warm-up, and outlier handling belong to Criterion rather than
//! simulator-side clocks or environment-controlled logging.

use std::{hint::black_box, time::Duration};

use celox::{EventHandle, Simulator, SimulatorBuilder, TieredExecutionTier, TieredPromotionStatus};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

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

const SORTER_SOURCE: &str = concat!(
    include_str!("../tests/fixtures/sorter_tree/sorter_item.veryl"),
    include_str!("../tests/fixtures/sorter_tree/dist_entry.veryl"),
    include_str!("../tests/fixtures/sorter_tree/min_reduction_tree.veryl"),
    include_str!("../tests/fixtures/sorter_tree/linear_sorter_pull.veryl"),
    include_str!("../tests/fixtures/sorter_tree/linear_sorter.veryl"),
    include_str!("../tests/fixtures/sorter_tree/sorter_tree.veryl"),
);

fn lcg_builder() -> SimulatorBuilder<'static> {
    Simulator::builder(LCG_SOURCE, "TieredBenchTop")
}

fn sorter_builder() -> SimulatorBuilder<'static> {
    Simulator::builder(SORTER_SOURCE, "SorterTreeDistEntry")
        .param("N", 8)
        .param("LEAF_DEPTH", 4)
        .param("OUT_DEPTH", 16)
        .reset_type(celox::ResetType::AsyncHigh)
}

fn benchmark_startup(c: &mut Criterion) {
    let mut group = c.benchmark_group("tiered/startup");

    group.bench_function("lcg/interpreter", |b| {
        b.iter(|| black_box(lcg_builder().build_interpreter().unwrap()));
    });
    group.bench_function("lcg/native", |b| {
        b.iter(|| black_box(lcg_builder().build_native().unwrap()));
    });
    group.bench_function("lcg/tiered", |b| {
        b.iter(|| {
            let sim = lcg_builder().build_tiered().unwrap();
            black_box(sim.tiered_execution_stats());
            black_box(sim)
        });
    });

    group.sample_size(10);
    group.bench_function("sorter_n8/interpreter", |b| {
        b.iter(|| black_box(sorter_builder().build_interpreter().unwrap()));
    });
    group.bench_function("sorter_n8/native", |b| {
        b.iter(|| black_box(sorter_builder().build_native().unwrap()));
    });
    group.bench_function("sorter_n8/tiered", |b| {
        b.iter(|| black_box(sorter_builder().build_tiered().unwrap()));
    });

    group.finish();
}

fn benchmark_time_to_compiled(c: &mut Criterion) {
    let mut group = c.benchmark_group("tiered/time_to_compiled");
    group.sample_size(10);

    for (name, builder, limit) in [
        (
            "lcg",
            lcg_builder as fn() -> SimulatorBuilder<'static>,
            1_000_000u64,
        ),
        (
            "sorter_n8",
            sorter_builder as fn() -> SimulatorBuilder<'static>,
            20_000u64,
        ),
    ] {
        group.bench_with_input(BenchmarkId::from_parameter(name), &limit, |b, &limit| {
            b.iter(|| {
                let mut sim = builder().build_tiered().unwrap();
                let clk = sim.event("clk");
                while !sim.is_compiled()
                    && sim.tiered_execution_stats().interpreted_evaluations < limit
                {
                    sim.tick(clk).unwrap();
                }
                let stats = sim.tiered_execution_stats();
                assert_ne!(stats.promotion, TieredPromotionStatus::Failed);
                assert_eq!(stats.tier, TieredExecutionTier::Compiled);
                assert!(stats.promoted_after_interpreted_evaluations.is_some());
                black_box(stats)
            });
        });
    }

    group.finish();
}

fn benchmark_steady_state(c: &mut Criterion) {
    const BATCH: u32 = 1_000;
    let mut group = c.benchmark_group("tiered/steady_state");
    group.throughput(Throughput::Elements(u64::from(BATCH)));

    let mut native = lcg_builder().build_native().unwrap();
    let native_clk = native.event("clk");
    group.bench_function("lcg/native", |b| {
        b.iter(|| {
            native
                .tick_by_id_n(black_box(native_clk.id()), BATCH)
                .unwrap()
        });
    });

    let mut interpreter = lcg_builder().build_interpreter().unwrap();
    let interpreter_clk = interpreter.event("clk");
    group.bench_function("lcg/interpreter", |b| {
        b.iter(|| {
            interpreter
                .tick_by_id_n(black_box(interpreter_clk.id()), BATCH)
                .unwrap()
        });
    });

    let mut tiered = lcg_builder().build_tiered().unwrap();
    let tiered_clk = tiered.event("clk");
    while !tiered.is_compiled()
        && tiered.tiered_execution_stats().interpreted_evaluations < 1_000_000
    {
        tiered.tick(tiered_clk).unwrap();
    }
    assert_eq!(
        tiered.tiered_execution_stats().tier,
        TieredExecutionTier::Compiled
    );
    group.bench_function("lcg/tiered_promoted", |b| {
        b.iter(|| {
            tiered
                .tick_by_id_n(black_box(tiered_clk.id()), BATCH)
                .unwrap()
        });
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = benchmark_startup, benchmark_time_to_compiled, benchmark_steady_state
}
criterion_main!(benches);
