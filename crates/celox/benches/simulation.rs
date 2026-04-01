use celox::{DeadStorePolicy, Simulator, TestResult};
use criterion::{Criterion, criterion_group, criterion_main};

// Wrapper modules are shared with celox-bench-sv (Verilator SV generation)
// via benches/veryl/*.veryl to guarantee identical circuits.

const CODE: &str = include_str!("../../../benches/veryl/top_n1000.veryl");

// Native testbench sources (DUT + TB module in one string)
const NATIVE_TB_COUNTER_N1000: &str = concat!(
    include_str!("../../../benches/veryl/top_n1000.veryl"),
    include_str!("../../../benches/veryl/native_tb_counter_n1000.veryl"),
);
const NATIVE_TB_STD_COUNTER: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/counter/counter.veryl"),
    include_str!("../../../benches/veryl/std_counter_top.veryl"),
    include_str!("../../../benches/veryl/native_tb_std_counter.veryl"),
);

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

// std::fifo WIDTH=8 DEPTH=16
const FIFO_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/ram/ram.veryl"),
    include_str!("../../../deps/veryl/crates/std/veryl/src/fifo/fifo_controller.veryl"),
    include_str!("../../../deps/veryl/crates/std/veryl/src/fifo/fifo.veryl"),
    include_str!("../../../benches/veryl/fifo_top.veryl"),
);

// std::gray_encoder + gray_decoder WIDTH=32 (combinational roundtrip)
const GRAY_CODEC_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/gray/gray_encoder.veryl"),
    include_str!("../../../deps/veryl/crates/std/veryl/src/gray/gray_decoder.veryl"),
    include_str!("../../../benches/veryl/gray_codec_top.veryl"),
);

// std::edge_detector WIDTH=32
const EDGE_DETECTOR_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/edge_detector/edge_detector.veryl"),
    include_str!("../../../benches/veryl/edge_detector_top.veryl"),
);

// std::onehot W=64
const ONEHOT_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/countones/onehot.veryl"),
    include_str!("../../../benches/veryl/onehot_top.veryl"),
);

// std::lfsr_galois SIZE=32
const LFSR_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/lfsr/lfsr_galois.veryl"),
    include_str!("../../../benches/veryl/lfsr_top.veryl"),
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

fn benchmark_fifo(c: &mut Criterion) {
    c.bench_function("simulation_build_fifo_w8_d16", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(FIFO_SRC, "Top").build().unwrap();
        })
    });

    let mut sim = Simulator::builder(FIFO_SRC, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_push = sim.signal("i_push");
    let i_data = sim.signal("i_data");
    let i_pop = sim.signal("i_pop");
    let o_data = sim.signal("o_data");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_push, 0u8);
        io.set(i_pop, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Alternating push/pop pattern
    c.bench_function("simulation_tick_fifo_w8_d16_x1", |b| {
        let mut push = true;
        b.iter(|| {
            sim.modify(|io| {
                io.set(i_push, if push { 1u8 } else { 0u8 });
                io.set(i_pop, if push { 0u8 } else { 1u8 });
                io.set(i_data, 0xAAu8);
            })
            .unwrap();
            sim.tick(clk).unwrap();
            push = !push;
        })
    });

    c.bench_function("testbench_tick_fifo_w8_d16_x1000000", |b| {
        let mut push = true;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| {
                    io.set(i_push, if push { 1u8 } else { 0u8 });
                    io.set(i_pop, if push { 0u8 } else { 1u8 });
                    io.set(i_data, 0xAAu8);
                })
                .unwrap();
                sim.tick(clk).unwrap();
                std::hint::black_box(sim.get(o_data));
                push = !push;
            }
        })
    });
}

fn benchmark_gray_codec(c: &mut Criterion) {
    c.bench_function("simulation_build_gray_codec_w32", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(GRAY_CODEC_SRC, "Top").build().unwrap();
        })
    });

    let mut sim = Simulator::builder(GRAY_CODEC_SRC, "Top").build().unwrap();
    let i_bin = sim.signal("i_bin");
    let o_bin = sim.signal("o_bin");

    c.bench_function("simulation_eval_gray_codec_w32_x1", |b| {
        let mut input: u32 = 0;
        b.iter(|| {
            sim.modify(|io| io.set(i_bin, input)).unwrap();
            std::hint::black_box(sim.get(o_bin));
            input = input.wrapping_add(1);
        })
    });

    c.bench_function("simulation_eval_gray_codec_w32_x1000000", |b| {
        let mut input: u32 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| io.set(i_bin, input)).unwrap();
                std::hint::black_box(sim.get(o_bin));
                input = input.wrapping_add(1);
            }
        })
    });
}

fn benchmark_edge_detector(c: &mut Criterion) {
    c.bench_function("simulation_build_edge_detector_w32", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(EDGE_DETECTOR_SRC, "Top")
                .build()
                .unwrap();
        })
    });

    let mut sim = Simulator::builder(EDGE_DETECTOR_SRC, "Top")
        .build()
        .unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_data = sim.signal("i_data");
    let o_posedge = sim.signal("o_posedge");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_data, 0u32);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    c.bench_function("simulation_tick_edge_detector_w32_x1", |b| {
        let mut input: u32 = 0;
        b.iter(|| {
            sim.modify(|io| io.set(i_data, input)).unwrap();
            sim.tick(clk).unwrap();
            input = input.wrapping_add(1);
        })
    });

    c.bench_function("testbench_tick_edge_detector_w32_x1000000", |b| {
        let mut input: u32 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| io.set(i_data, input)).unwrap();
                sim.tick(clk).unwrap();
                std::hint::black_box(sim.get(o_posedge));
                input = input.wrapping_add(1);
            }
        })
    });
}

fn benchmark_onehot(c: &mut Criterion) {
    c.bench_function("simulation_build_onehot_w64", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(ONEHOT_SRC, "Top").build().unwrap();
        })
    });

    let mut sim = Simulator::builder(ONEHOT_SRC, "Top").build().unwrap();
    let i_data = sim.signal("i_data");
    let o_onehot = sim.signal("o_onehot");

    c.bench_function("simulation_eval_onehot_w64_x1", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            sim.modify(|io| io.set(i_data, input)).unwrap();
            std::hint::black_box(sim.get(o_onehot));
            input = input.wrapping_add(1);
        })
    });

    c.bench_function("simulation_eval_onehot_w64_x1000000", |b| {
        let mut input: u64 = 0;
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.modify(|io| io.set(i_data, input)).unwrap();
                std::hint::black_box(sim.get(o_onehot));
                input = input.wrapping_add(1);
            }
        })
    });
}

fn benchmark_lfsr(c: &mut Criterion) {
    c.bench_function("simulation_build_lfsr_w32", |b| {
        b.iter(|| {
            let _sim = Simulator::builder(LFSR_SRC, "Top").build().unwrap();
        })
    });

    let mut sim = Simulator::builder(LFSR_SRC, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_en = sim.signal("i_en");
    let i_set = sim.signal("i_set");
    let i_setval = sim.signal("i_setval");
    let o_val = sim.signal("o_val");

    // Reset + seed
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_en, 1u8);
        io.set(i_set, 1u8);
        io.set(i_setval, 1u32);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(i_set, 0u8);
    })
    .unwrap();

    c.bench_function("simulation_tick_lfsr_w32_x1", |b| {
        b.iter(|| {
            sim.tick(clk).unwrap();
        })
    });

    c.bench_function("simulation_tick_lfsr_w32_x1000000", |b| {
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.tick(clk).unwrap();
            }
        })
    });

    c.bench_function("testbench_tick_lfsr_w32_x1000000", |b| {
        b.iter(|| {
            for _ in 0..1_000_000 {
                sim.tick(clk).unwrap();
                std::hint::black_box(sim.get(o_val));
            }
        })
    });
}

// ── Native testbench benchmarks ────────────────────────────────────────

fn benchmark_native_tb_counter(c: &mut Criterion) {
    // Build cost: compile + run_test (includes compilation)
    c.bench_function("native_tb_build_counter_n1000", |b| {
        b.iter(|| {
            let _result = Simulator::builder(NATIVE_TB_COUNTER_N1000, "bench_counter_n1000")
                .build()
                .unwrap();
        })
    });

    // Full run_test: compile + reset + 1M ticks + $finish
    c.bench_function("native_tb_run_counter_n1000_x1000000", |b| {
        b.iter(|| {
            let result = Simulator::builder(NATIVE_TB_COUNTER_N1000, "bench_counter_n1000")
                .run_test()
                .unwrap();
            assert_eq!(result, TestResult::Pass);
        })
    });

    // Tick-only: pre-built simulator, measure just the testbench execution
    let mut sim = Simulator::builder(NATIVE_TB_COUNTER_N1000, "bench_counter_n1000")
        .build()
        .unwrap();
    let initial_stmts = sim.program().initial_statements.clone().unwrap();
    let mut tb_builder = celox::testbench::TestbenchBuilder::new(&sim);
    tb_builder.build_event_map(&initial_stmts);
    let tb_stmts = tb_builder.convert(&initial_stmts);

    c.bench_function("native_tb_exec_counter_n1000_x1000000", |b| {
        b.iter(|| {
            let result = celox::testbench::run_testbench(&mut sim, &tb_stmts);
            assert_eq!(result, TestResult::Pass);
        })
    });

}

fn benchmark_native_tb_std_counter(c: &mut Criterion) {
    c.bench_function("native_tb_run_std_counter_w32_x1000000", |b| {
        b.iter(|| {
            let result = Simulator::builder(NATIVE_TB_STD_COUNTER, "bench_std_counter")
                .run_test()
                .unwrap();
            assert_eq!(result, TestResult::Pass);
        })
    });

    let mut sim = Simulator::builder(NATIVE_TB_STD_COUNTER, "bench_std_counter")
        .build()
        .unwrap();
    let initial_stmts = sim.program().initial_statements.clone().unwrap();
    let mut tb_builder = celox::testbench::TestbenchBuilder::new(&sim);
    tb_builder.build_event_map(&initial_stmts);
    let tb_stmts = tb_builder.convert(&initial_stmts);

    c.bench_function("native_tb_exec_std_counter_w32_x1000000", |b| {
        b.iter(|| {
            let result = celox::testbench::run_testbench(&mut sim, &tb_stmts);
            assert_eq!(result, TestResult::Pass);
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
    benchmark_fifo,
    benchmark_gray_codec,
    benchmark_edge_detector,
    benchmark_onehot,
    benchmark_lfsr,
    benchmark_native_tb_counter,
    benchmark_native_tb_std_counter,
);
criterion_main!(benches);
