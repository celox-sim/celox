/**
 * Verilator benchmark harness – mirrors the Celox Criterion benchmarks.
 * Uses Google Benchmark for timing.
 */

#include <benchmark/benchmark.h>
#include "VTop.h"
#include "verilated.h"
#include <chrono>
#include <cstdint>

static void reset(VTop *top) {
    top->rst = 0;     // assert reset (active-low)
    top->clk = 0; top->eval();
    top->clk = 1; top->eval();
    top->rst = 1;     // release reset
    top->clk = 0; top->eval();
}

static inline void tick(VTop *top) {
    top->clk = 0; top->eval();
    top->clk = 1; top->eval();
}

// --- simulation_tick_top_n1000_x1 ---
static void BM_simulation_tick_x1(benchmark::State &state) {
    VTop top;
    reset(&top);
    for (int i = 0; i < 10000; i++) tick(&top);  // warm up
    for (auto _ : state)
        tick(&top);
}
BENCHMARK(BM_simulation_tick_x1)
    ->Name("simulation_tick_top_n1000_x1")
    ->Unit(benchmark::kNanosecond);

// --- simulation_tick_top_n1000_x1000000 ---
static void BM_simulation_tick_x1000000(benchmark::State &state) {
    VTop top;
    reset(&top);
    for (auto _ : state) {
        auto t0 = std::chrono::high_resolution_clock::now();
        for (int i = 0; i < 1000000; i++) tick(&top);
        auto t1 = std::chrono::high_resolution_clock::now();
        state.SetIterationTime(std::chrono::duration<double>(t1 - t0).count());
    }
}
BENCHMARK(BM_simulation_tick_x1000000)
    ->Name("simulation_tick_top_n1000_x1000000")
    ->UseManualTime()->Iterations(3)
    ->Unit(benchmark::kNanosecond);

// --- testbench_tick_top_n1000_x1 ---
static void BM_testbench_tick_x1(benchmark::State &state) {
    VTop top;
    reset(&top);
    for (int i = 0; i < 10000; i++) tick(&top);  // warm up
    for (auto _ : state) {
        tick(&top);
        benchmark::DoNotOptimize(top.cnt0);
    }
}
BENCHMARK(BM_testbench_tick_x1)
    ->Name("testbench_tick_top_n1000_x1")
    ->Unit(benchmark::kNanosecond);

// --- testbench_tick_top_n1000_x1000000 ---
static void BM_testbench_tick_x1000000(benchmark::State &state) {
    VTop top;
    reset(&top);
    for (auto _ : state) {
        volatile uint32_t sink = 0;
        auto t0 = std::chrono::high_resolution_clock::now();
        for (int i = 0; i < 1000000; i++) {
            tick(&top);
            sink = top.cnt0;
        }
        auto t1 = std::chrono::high_resolution_clock::now();
        (void)sink;
        state.SetIterationTime(std::chrono::duration<double>(t1 - t0).count());
    }
}
BENCHMARK(BM_testbench_tick_x1000000)
    ->Name("testbench_tick_top_n1000_x1000000")
    ->UseManualTime()->Iterations(3)
    ->Unit(benchmark::kNanosecond);

BENCHMARK_MAIN();
