// CodSpeed's Criterion harness reads its own environment during macro expansion.
#![allow(clippy::disallowed_methods)]

use celox::Simulator;
use codspeed_criterion_compat::{Criterion, black_box, criterion_group, criterion_main};

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

criterion_group!(benches, benchmark_compilation);
criterion_main!(benches);
