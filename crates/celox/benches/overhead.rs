use criterion::{Criterion, criterion_group, criterion_main};
use celox::{Simulation, Simulator};

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

fn benchmark_simulation_overhead(c: &mut Criterion) {
    // Benchmark 1: Raw Simulator::tick
    {
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

        c.bench_function("simulator_tick_x10000", |b| {
            b.iter(|| {
                for _ in 0..10000 {
                    sim.tick(clk).unwrap();
                }
            })
        });
    }

    // Benchmark 2: Simulation::step
    {
        let mut sim = Simulation::builder(CODE, "Top").build().unwrap();
        sim.add_clock("clk", 10, 10);

        // Reset
        // simulation currently doesn't have a direct reset method,
        // but we can schedule events or just let it run.
        // For this benchmark, we just want to measure the steady state overhead.

        c.bench_function("simulation_step_x20000", |b| {
            b.iter(|| {
                // 20000 steps = 10000 cycles (rising + falling)
                for _ in 0..20000 {
                    sim.step().unwrap();
                }
            })
        });
    }
}

criterion_group!(benches, benchmark_simulation_overhead);
criterion_main!(benches);



