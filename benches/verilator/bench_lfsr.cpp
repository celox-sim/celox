/**
 * Verilator benchmark harness for lfsr_galois (SIZE=32).
 * Mirrors Celox benchmark_lfsr. Sequential (no reset, uses i_set).
 */

#include <benchmark/benchmark.h>
#include "VTop.h"
#include "verilated.h"
#include <chrono>
#include <cstdint>

static void seed(VTop *top) {
    // LFSR has no reset port. Use i_set to seed.
    // The Top module has rst but LFSR ignores it.
    top->rst = 0;
    top->i_en = 1;
    top->i_set = 1;
    top->i_setval = 1;
    top->clk = 0; top->eval();
    top->clk = 1; top->eval();
    top->rst = 1;
    top->i_set = 0;
    top->clk = 0; top->eval();
}

static inline void tick(VTop *top) {
    top->clk = 0; top->eval();
    top->clk = 1; top->eval();
}

// --- simulation_tick_lfsr_w32_x1 ---
static void BM_tick_x1(benchmark::State &state) {
    VTop top;
    seed(&top);
    for (auto _ : state)
        tick(&top);
}
BENCHMARK(BM_tick_x1)
    ->Name("simulation_tick_lfsr_w32_x1")
    ->Unit(benchmark::kNanosecond);

// --- simulation_tick_lfsr_w32_x1000000 ---
static void BM_tick_x1000000(benchmark::State &state) {
    VTop top;
    seed(&top);
    for (auto _ : state) {
        auto t0 = std::chrono::high_resolution_clock::now();
        for (int i = 0; i < 1000000; i++) tick(&top);
        auto t1 = std::chrono::high_resolution_clock::now();
        state.SetIterationTime(std::chrono::duration<double>(t1 - t0).count());
    }
}
BENCHMARK(BM_tick_x1000000)
    ->Name("simulation_tick_lfsr_w32_x1000000")
    ->UseManualTime()->Iterations(3)
    ->Unit(benchmark::kNanosecond);

// --- testbench_tick_lfsr_w32_x1000000 ---
static void BM_testbench_tick_x1000000(benchmark::State &state) {
    VTop top;
    seed(&top);
    for (auto _ : state) {
        volatile uint32_t sink = 0;
        auto t0 = std::chrono::high_resolution_clock::now();
        for (int i = 0; i < 1000000; i++) {
            tick(&top);
            sink = top.o_val;
        }
        auto t1 = std::chrono::high_resolution_clock::now();
        (void)sink;
        state.SetIterationTime(std::chrono::duration<double>(t1 - t0).count());
    }
}
BENCHMARK(BM_testbench_tick_x1000000)
    ->Name("testbench_tick_lfsr_w32_x1000000")
    ->UseManualTime()->Iterations(3)
    ->Unit(benchmark::kNanosecond);

BENCHMARK_MAIN();
