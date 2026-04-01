/**
 * Verilator benchmark harness for edge_detector (WIDTH=32).
 * Mirrors Celox benchmark_edge_detector. Sequential.
 *
 * Reset polarity: AsyncLow (rst=0 asserts reset, rst=1 releases).
 */

#include <benchmark/benchmark.h>
#include "VTop.h"
#include "verilated.h"
#include <chrono>
#include <cstdint>

static void reset(VTop *top) {
    top->rst = 0;
    top->i_data = 0;
    top->clk = 0; top->eval();
    top->clk = 1; top->eval();
    top->rst = 1;
    top->clk = 0; top->eval();
}

static inline void tick(VTop *top) {
    top->clk = 0; top->eval();
    top->clk = 1; top->eval();
}

// --- simulation_tick_edge_detector_w32_x1 ---
static void BM_tick_x1(benchmark::State &state) {
    VTop top;
    reset(&top);
    uint32_t input = 0;
    for (auto _ : state) {
        top.i_data = input++;
        tick(&top);
    }
}
BENCHMARK(BM_tick_x1)
    ->Name("simulation_tick_edge_detector_w32_x1")
    ->Unit(benchmark::kNanosecond);

// --- testbench_tick_edge_detector_w32_x1000000 ---
static void BM_testbench_tick_x1000000(benchmark::State &state) {
    VTop top;
    reset(&top);
    uint32_t input = 0;
    for (auto _ : state) {
        volatile uint32_t sink = 0;
        auto t0 = std::chrono::high_resolution_clock::now();
        for (int i = 0; i < 1000000; i++) {
            top.i_data = input++;
            tick(&top);
            sink = top.o_posedge;
        }
        auto t1 = std::chrono::high_resolution_clock::now();
        (void)sink;
        state.SetIterationTime(std::chrono::duration<double>(t1 - t0).count());
    }
}
BENCHMARK(BM_testbench_tick_x1000000)
    ->Name("testbench_tick_edge_detector_w32_x1000000")
    ->UseManualTime()->Iterations(3)
    ->Unit(benchmark::kNanosecond);

BENCHMARK_MAIN();
