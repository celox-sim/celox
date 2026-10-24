// CodSpeed's Criterion harness reads its own environment during macro expansion.
#![allow(clippy::disallowed_methods)]

use celox::Simulator;
use codspeed_criterion_compat::{
    Criterion, IterManualOptions, black_box, criterion_group, criterion_main,
};

#[path = "../tests/test_utils/nested_write.rs"]
mod nested_write;
#[path = "../tests/test_utils/sorter_tree.rs"]
mod sorter_tree;

const COUNTER: &str = include_str!("../testdata/veryl/top_n1000.veryl");
const AXI_LITE_REG_FILE: &str = include_str!("../tests/fixtures/bitslice/axi_lite_reg_file.veryl");

fn compile_default(source: &str, top: &str, parameter: Option<(&str, u64)>) {
    let mut builder = Simulator::builder(black_box(source), top);
    if let Some((name, value)) = parameter {
        builder = builder.param(name, value);
    }
    black_box(builder.build().expect("benchmark design should compile"));
}

fn benchmark_compilation(c: &mut Criterion) {
    // Initialize process-global analyzer state outside CodSpeed's measured spans.
    // Otherwise the first benchmark alone would include one-time setup work.
    compile_default(COUNTER, "Top", Some(("N", 1)));

    let mut group = c.benchmark_group("compile/default");

    group.bench_function("counter_n1", |b| {
        b.iter(|| compile_default(COUNTER, "Top", Some(("N", 1))));
    });

    group.bench_function("axi_lite_reg_file", |b| {
        b.iter(|| compile_default(AXI_LITE_REG_FILE, "AxiLiteRegFile", None));
    });

    group.bench_function("counter_n1000", |b| {
        b.iter(|| compile_default(COUNTER, "Top", Some(("N", 1_000))));
    });

    group.finish();

    c.bench_function("compile/cranelift/counter_n1000", |b| {
        b.iter(|| {
            let simulator = Simulator::builder(black_box(COUNTER), "Top")
                .param("N", 1_000)
                .build_cranelift()
                .expect("benchmark design should compile with Cranelift");
            black_box(simulator);
        });
    });
}

fn benchmark_scaling(c: &mut Criterion) {
    // Warm analyzer state before any measured span, including filtered runs.
    compile_default(COUNTER, "Top", Some(("N", 1)));
    // Each large design runs once: no five-run warmup or adaptive resampling.
    // CodSpeed simulation measures one iteration; walltime uses the same schedule.
    for n in [4, 8, 16, 32] {
        c.bench_function(&format!("compile/sorter_tree/n{n}"), |b| {
            b.iter_manual_unstable(IterManualOptions::new(), || {
                black_box(sorter_tree::compile(black_box(n)))
            });
        });
    }
    macro_rules! backend {
        ($name:literal, $build:ident) => {
            for four_state in [false, true] {
                let state = if four_state { "4state" } else { "2state" };
                for width in [257, 512] {
                    c.bench_function(
                        &format!("scaling/nested_write/{}/{state}/complete_{width}", $name),
                        |b| {
                            b.iter_manual_unstable(IterManualOptions::new(), || {
                                nested_write::complete!($build, black_box(width), four_state)
                            })
                        },
                    );
                }
                c.bench_function(
                    &format!("scaling/nested_write/{}/{state}/incomplete_512", $name),
                    |b| {
                        b.iter_manual_unstable(IterManualOptions::new(), || {
                            nested_write::incomplete!($build, black_box(512), four_state)
                        })
                    },
                );
            }
        };
    }
    backend!("interpreter", build_interpreter);
    backend!("native", build_native);
    backend!("cranelift", build_cranelift);
    c.bench_function("scaling/nested_write/strict_reject_512", |b| {
        b.iter_manual_unstable(IterManualOptions::new(), || {
            nested_write::strictly_reject(black_box(512))
        });
    });
}

criterion_group!(benches, benchmark_compilation, benchmark_scaling);
criterion_main!(benches);
