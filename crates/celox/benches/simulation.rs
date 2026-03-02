use criterion::{Criterion, criterion_group, criterion_main};
use celox::Simulator;

// P=6: K=63-bit codeword, N=57-bit data
const LINEAR_SEC_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/coding/linear_sec_encoder.veryl"),
    include_str!("../../../deps/veryl/crates/std/veryl/src/coding/linear_sec_decoder.veryl"),
    r#"
module Top #(
    param P: u32 = 6,
    const K: u32 = (1 << P) - 1,
    const N: u32 = K - P,
)(
    i_word     : input  logic<N>,
    o_codeword : output logic<K>,
    o_word     : output logic<N>,
    o_corrected: output logic,
) {
    inst u_enc: linear_sec_encoder #(
        P: P,
    ) (
        i_word,
        o_codeword,
    );
    inst u_dec: linear_sec_decoder #(
        P: P,
    ) (
        i_codeword: o_codeword,
        o_word,
        o_corrected,
    );
}
"#
);

const CODE: &str = r#"
    module Top #(
        param N: u32 = 1000,
    )(
        clk: input clock,
        rst: input reset,
        cnt: output logic<32>[N],
        cnt0: output logic<32>,
    ) {
        assign cnt0 = cnt[0];
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
    let cnt0 = sim.signal("cnt0");

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 0u8)).unwrap();

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

    // Testbench pattern: tick + read output
    c.bench_function("testbench_tick_top_n1000_x1", |b| {
        b.iter(|| {
            sim.tick(clk).unwrap();
            std::hint::black_box(sim.get(cnt0));
        })
    });

    c.bench_function("testbench_tick_top_n1000_x1000000", |b| {
        b.iter(|| {
            for _ in 0..1000000 {
                sim.tick(clk).unwrap();
                std::hint::black_box(sim.get(cnt0));
            }
        })
    });
}

fn benchmark_linear_sec(c: &mut Criterion) {
    c.bench_function("simulation_build_linear_sec_p6", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(LINEAR_SEC_SRC, "Top").build().unwrap();
        })
    });

    let mut sim = Simulator::builder(LINEAR_SEC_SRC, "Top").build().unwrap();
    let i_word      = sim.signal("i_word");
    let o_word      = sim.signal("o_word");
    let o_corrected = sim.signal("o_corrected");

    c.bench_function("simulation_eval_linear_sec_p6_x1", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            sim.modify(|io| io.set(i_word, input)).unwrap();
            std::hint::black_box(sim.get(o_word));
            input = input.wrapping_add(1);
        })
    });

    c.bench_function("simulation_eval_linear_sec_p6_x1000000", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| io.set(i_word, input)).unwrap();
                std::hint::black_box(sim.get(o_word));
                input = input.wrapping_add(1);
            }
        })
    });

    // Testbench pattern: encode + decode + check corrected flag
    c.bench_function("testbench_eval_linear_sec_p6_x1000000", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| io.set(i_word, input)).unwrap();
                std::hint::black_box(sim.get(o_corrected));
                input = input.wrapping_add(1);
            }
        })
    });
}

criterion_group!(benches, benchmark_counter, benchmark_linear_sec);
criterion_main!(benches);
