use criterion::{Criterion, criterion_group, criterion_main};
use celox::Simulator;

const CODE: &str = r#"
    module Top #(
        param N: u32 = 1000,
    )(
        clk: input clock,
        rst: input reset,
        cnt: output logic<32>[N],
    ) {
        for i in 0..N: g {
            always_ff (clk, rst) {
                if_reset {
                    cnt[i] = 0;
                } else {
                    cnt[i] += 1;
                }
            }
        }
    }
    "#;

fn benchmark_counter(c: &mut Criterion) {
    c.bench_function("simulation_build_top_n1000", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(CODE, "Top").build().unwrap();
        })
    });

    let mut sim = Simulator::builder(CODE, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");

    sim.modify(|io| {
        io.set(rst, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| {
        io.set(rst, 0u8);
    })
    .unwrap();

    c.bench_function("simulation_tick_top_n1000_x1", |b| {
        b.iter(|| {
            sim.tick(clk).unwrap();
        })
    });

    c.bench_function("simulation_tick_top_n1000_x1000000", |b| {
        b.iter(|| {
            for _ in 0..1000000 {
                sim.tick(clk).unwrap();
            }
        })
    });
}

criterion_group!(benches, benchmark_counter);
criterion_main!(benches);
