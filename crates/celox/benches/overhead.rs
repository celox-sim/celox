use celox::{Simulation, Simulator};
use criterion::{Criterion, criterion_group, criterion_main};

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

        // AsyncLow reset: active at 0, inactive at 1
        sim.modify(|io| {
            io.set(rst, 0u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| {
            io.set(rst, 1u8);
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

    // Benchmark 1b/1c: Simulator::tick with a derived clock domain: a gated
    // clock (`clk & en`) and a register-driven divider.
    for (name, derived) in [
        (
            "simulator_tick_gated_x10000",
            "let gated: '_ clock = clk & en; always_ff (gated) { extra += 1; }",
        ),
        (
            "simulator_tick_divided_x10000",
            "var half: logic; always_ff (clk) { half = ~half; }
             let divided: '_ clock = half; always_ff (divided) { extra += 1; }",
        ),
    ] {
        let code = CODE.replace(
            "cnt: output logic<32>[N],\n    ) {",
            &format!(
                "cnt: output logic<32>[N],\n        en: input logic,\n        extra: output logic<32>,\n    ) {{\n        {derived}"
            ),
        );
        let mut sim = Simulator::builder(&code, "Top").build().unwrap();
        let clk = sim.event("clk");
        let rst = sim.signal("rst");
        let en = sim.signal("en");
        sim.modify(|io| {
            io.set(rst, 0u8);
            io.set(en, 1u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(rst, 1u8)).unwrap();

        c.bench_function(name, |b| {
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

    // Benchmark 3: Simulation::step of SystemVerilog counters at the rising
    // edge only, half at each edge of `clk`, and half at the falling edge of
    // a copy of `clk`.
    #[cfg(feature = "systemverilog")]
    for (name, falling) in [
        ("sv_simulation_step_posedge_x20000", "posedge clk"),
        ("sv_simulation_step_both_edges_x20000", "negedge clk"),
        ("sv_simulation_step_negedge_copy_x20000", "negedge c"),
    ] {
        let source = format!(
            "module Top #(parameter int N = 1000) (input logic clk, input logic rst,
                                                  output logic [31:0] cnt [N]);
                 logic c;
                 assign c = clk;
                 for (genvar i = 0; i < N; i++) begin : g
                     if (i % 2 == 0) begin : rise
                         always_ff @(posedge clk or posedge rst)
                             if (rst) cnt[i] <= 0; else cnt[i] <= cnt[i] + 1;
                     end else begin : fall
                         always_ff @({falling} or posedge rst)
                             if (rst) cnt[i] <= 0; else cnt[i] <= cnt[i] + 1;
                     end
                 end
             endmodule"
        );
        let path = std::path::Path::new("overhead.sv");
        let mut sim =
            match Simulation::from_sv_sources(vec![(source.as_str(), path)], "Top").build() {
                Ok(sim) => sim,
                Err(error) => {
                    eprintln!("{name}: {error}");
                    continue;
                }
            };
        sim.schedule("rst", 0, 1).unwrap();
        sim.step().unwrap();
        sim.schedule("rst", 1, 0).unwrap();
        sim.step().unwrap();
        sim.add_clock("clk", 10, 10);

        c.bench_function(name, |b| {
            b.iter(|| {
                for _ in 0..20000 {
                    sim.step().unwrap();
                }
            })
        });
    }
}

criterion_group!(benches, benchmark_simulation_overhead);
criterion_main!(benches);
