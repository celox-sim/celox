use celox::{DeadStorePolicy, Simulator};
use criterion::{Criterion, criterion_group, criterion_main};

// Wrapper modules are shared with celox-bench-sv (Verilator SV generation)
// via benches/veryl/*.veryl to guarantee identical circuits.

const CODE: &str = include_str!("../../../benches/veryl/top_n1000.veryl");

// P=6: K=63-bit codeword, N=57-bit data
const LINEAR_SEC_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/coding/linear_sec_encoder.veryl"),
    include_str!("../../../deps/veryl/crates/std/veryl/src/coding/linear_sec_decoder.veryl"),
    include_str!("../../../benches/veryl/linear_sec_top.veryl"),
);

// std::countones W=64: recursive combinational popcount tree
const COUNTONES_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/countones/countones.veryl"),
    include_str!("../../../benches/veryl/countones_top.veryl"),
);

// std::counter WIDTH=32
const STD_COUNTER_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/counter/counter.veryl"),
    include_str!("../../../benches/veryl/std_counter_top.veryl"),
);

// std::gray_counter WIDTH=32
const GRAY_COUNTER_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/counter/counter.veryl"),
    include_str!("../../../deps/veryl/crates/std/veryl/src/gray/gray_encoder.veryl"),
    include_str!("../../../deps/veryl/crates/std/veryl/src/gray/gray_counter.veryl"),
    include_str!("../../../benches/veryl/gray_counter_top.veryl"),
);

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

    // AsyncLow reset: active at 0, inactive at 1
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

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
    let i_word = sim.signal("i_word");
    let o_word = sim.signal("o_word");
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

fn benchmark_linear_sec_isolation(c: &mut Criterion) {
    let mut sim = Simulator::builder(LINEAR_SEC_SRC, "Top").build().unwrap();
    let i_word = sim.signal("i_word");
    let o_word = sim.signal("o_word");

    // -- 1. Pure eval_comb (same input, no I/O overhead) --
    sim.modify(|io| io.set(i_word, 42u64)).unwrap();
    sim.eval_comb().unwrap();

    c.bench_function("isolation_eval_comb_linear_sec_p6", |b| {
        b.iter(|| {
            sim.eval_comb().unwrap();
        })
    });

    c.bench_function("isolation_eval_comb_linear_sec_p6_x1000000", |b| {
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.eval_comb().unwrap();
            }
        })
    });

    // -- 2. Raw pointer I/O + eval_comb (Verilator-equivalent) --
    let i_offset = i_word.offset;
    let o_offset = o_word.offset;

    c.bench_function("isolation_raw_io_eval_linear_sec_p6", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            let (ptr, _) = sim.memory_as_mut_ptr();
            unsafe {
                std::ptr::write(ptr.add(i_offset) as *mut u64, input);
            }
            sim.eval_comb().unwrap();
            let out: u64 = unsafe {
                let (ptr, _) = sim.memory_as_ptr();
                std::ptr::read(ptr.add(o_offset) as *const u64)
            };
            std::hint::black_box(out);
            input = input.wrapping_add(1);
        })
    });

    c.bench_function("isolation_raw_io_eval_linear_sec_p6_x1000000", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                let (ptr, _) = sim.memory_as_mut_ptr();
                unsafe {
                    std::ptr::write(ptr.add(i_offset) as *mut u64, input);
                }
                sim.eval_comb().unwrap();
                let out: u64 = unsafe {
                    let (ptr, _) = sim.memory_as_ptr();
                    std::ptr::read(ptr.add(o_offset) as *const u64)
                };
                std::hint::black_box(out);
                input = input.wrapping_add(1);
            }
        })
    });

    // -- 3. set (modify) + eval_comb (no get) --
    c.bench_function("isolation_set_eval_linear_sec_p6", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            sim.modify(|io| io.set(i_word, input)).unwrap();
            sim.eval_comb().unwrap();
            input = input.wrapping_add(1);
        })
    });

    c.bench_function("isolation_set_eval_linear_sec_p6_x1000000", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| io.set(i_word, input)).unwrap();
                sim.eval_comb().unwrap();
                input = input.wrapping_add(1);
            }
        })
    });

    // -- 4. set + eval_comb + get_as<u64> (stack read, no BigUint) --
    c.bench_function("isolation_set_eval_get_as_linear_sec_p6", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            sim.modify(|io| io.set(i_word, input)).unwrap();
            let out: u64 = sim.get_as(o_word);
            std::hint::black_box(out);
            input = input.wrapping_add(1);
        })
    });

    c.bench_function("isolation_set_eval_get_as_linear_sec_p6_x1000000", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| io.set(i_word, input)).unwrap();
                let out: u64 = sim.get_as(o_word);
                std::hint::black_box(out);
                input = input.wrapping_add(1);
            }
        })
    });
}

fn benchmark_countones(c: &mut Criterion) {
    c.bench_function("simulation_build_countones_w64", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(COUNTONES_SRC, "Top").build().unwrap();
        })
    });

    let mut sim = Simulator::builder(COUNTONES_SRC, "Top").build().unwrap();
    let i_data = sim.signal("i_data");
    let o_ones = sim.signal("o_ones");

    c.bench_function("simulation_eval_countones_w64_x1", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            sim.modify(|io| io.set(i_data, input)).unwrap();
            std::hint::black_box(sim.get(o_ones));
            input = input.wrapping_add(1);
        })
    });

    c.bench_function("simulation_eval_countones_w64_x1000000", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| io.set(i_data, input)).unwrap();
                std::hint::black_box(sim.get(o_ones));
                input = input.wrapping_add(1);
            }
        })
    });
}

fn benchmark_countones_dse(c: &mut Criterion) {
    c.bench_function("dse_build_countones_w64", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(COUNTONES_SRC, "Top")
                .dead_store_policy(DeadStorePolicy::PreserveTopPorts)
                .build()
                .unwrap();
        })
    });

    let mut sim = Simulator::builder(COUNTONES_SRC, "Top")
        .dead_store_policy(DeadStorePolicy::PreserveTopPorts)
        .build()
        .unwrap();
    let i_data = sim.signal("i_data");
    let o_ones = sim.signal("o_ones");

    c.bench_function("dse_eval_countones_w64_x1", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            sim.modify(|io| io.set(i_data, input)).unwrap();
            std::hint::black_box(sim.get(o_ones));
            input = input.wrapping_add(1);
        })
    });

    c.bench_function("dse_eval_countones_w64_x1000000", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| io.set(i_data, input)).unwrap();
                std::hint::black_box(sim.get(o_ones));
                input = input.wrapping_add(1);
            }
        })
    });
}

fn benchmark_linear_sec_dse(c: &mut Criterion) {
    c.bench_function("dse_build_linear_sec_p6", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(LINEAR_SEC_SRC, "Top")
                .dead_store_policy(DeadStorePolicy::PreserveTopPorts)
                .build()
                .unwrap();
        })
    });

    let mut sim = Simulator::builder(LINEAR_SEC_SRC, "Top")
        .dead_store_policy(DeadStorePolicy::PreserveTopPorts)
        .build()
        .unwrap();
    let i_word = sim.signal("i_word");
    let o_word = sim.signal("o_word");

    c.bench_function("dse_eval_linear_sec_p6_x1", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            sim.modify(|io| io.set(i_word, input)).unwrap();
            std::hint::black_box(sim.get(o_word));
            input = input.wrapping_add(1);
        })
    });

    // Pure eval_comb with DSE (no I/O overhead)
    sim.modify(|io| io.set(i_word, 42u64)).unwrap();
    sim.eval_comb().unwrap();
    c.bench_function("dse_isolation_eval_comb_linear_sec_p6", |b| {
        b.iter(|| {
            sim.eval_comb().unwrap();
        })
    });

    c.bench_function("dse_eval_linear_sec_p6_x1000000", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| io.set(i_word, input)).unwrap();
                std::hint::black_box(sim.get(o_word));
                input = input.wrapping_add(1);
            }
        })
    });
}

fn benchmark_std_counter(c: &mut Criterion) {
    c.bench_function("simulation_build_std_counter_w32", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(STD_COUNTER_SRC, "Top").build().unwrap();
        })
    });

    let mut sim = Simulator::builder(STD_COUNTER_SRC, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_up = sim.signal("i_up");
    let o_count = sim.signal("o_count");

    // AsyncLow reset: active at 0, inactive at 1
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_up, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(i_up, 1u8);
    })
    .unwrap();

    c.bench_function("simulation_tick_std_counter_w32_x1", |b| {
        b.iter(|| {
            sim.tick(clk).unwrap();
        })
    });

    c.bench_function("simulation_tick_std_counter_w32_x1000000", |b| {
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.tick(clk).unwrap();
            }
        })
    });

    // Testbench pattern: tick + read count
    c.bench_function("testbench_tick_std_counter_w32_x1000000", |b| {
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.tick(clk).unwrap();
                std::hint::black_box(sim.get(o_count));
            }
        })
    });
}

fn benchmark_gray_counter(c: &mut Criterion) {
    c.bench_function("simulation_build_gray_counter_w32", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(GRAY_COUNTER_SRC, "Top").build().unwrap();
        })
    });

    let mut sim = Simulator::builder(GRAY_COUNTER_SRC, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_up = sim.signal("i_up");
    let o_count = sim.signal("o_count");

    // AsyncLow reset: active at 0, inactive at 1
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_up, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(i_up, 1u8);
    })
    .unwrap();

    c.bench_function("simulation_tick_gray_counter_w32_x1", |b| {
        b.iter(|| {
            sim.tick(clk).unwrap();
        })
    });

    c.bench_function("simulation_tick_gray_counter_w32_x1000000", |b| {
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.tick(clk).unwrap();
            }
        })
    });

    // Testbench pattern: tick + read Gray-encoded count
    c.bench_function("testbench_tick_gray_counter_w32_x1000000", |b| {
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.tick(clk).unwrap();
                std::hint::black_box(sim.get(o_count));
            }
        })
    });
}

criterion_group!(
    benches,
    benchmark_counter,
    benchmark_linear_sec,
    benchmark_linear_sec_isolation,
    benchmark_countones,
    benchmark_std_counter,
    benchmark_gray_counter,
    benchmark_countones_dse,
    benchmark_linear_sec_dse,
);
criterion_main!(benches);
