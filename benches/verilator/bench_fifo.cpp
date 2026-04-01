/**
 * Verilator benchmark harness for std::fifo (WIDTH=8, DEPTH=16).
 * Mirrors Celox benchmark_fifo. Uses Google Benchmark for timing.
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
    top->i_push = 0;
    top->i_pop = 0;
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

// --- simulation_tick_fifo_w8_d16_x1 ---
static void BM_tick_x1(benchmark::State &state) {
    VTop top;
    reset(&top);
    bool push = true;
    for (auto _ : state) {
        top.i_push = push ? 1 : 0;
        top.i_pop = push ? 0 : 1;
        top.i_data = 0xAA;
        tick(&top);
        push = !push;
    }
}
BENCHMARK(BM_tick_x1)
    ->Name("simulation_tick_fifo_w8_d16_x1")
    ->Unit(benchmark::kNanosecond);

// --- testbench_tick_fifo_w8_d16_x1000000 ---
static void BM_testbench_tick_x1000000(benchmark::State &state) {
    VTop top;
    reset(&top);
    for (auto _ : state) {
        volatile uint8_t sink = 0;
        bool push = true;
        auto t0 = std::chrono::high_resolution_clock::now();
        for (int i = 0; i < 1000000; i++) {
            top.i_push = push ? 1 : 0;
            top.i_pop = push ? 0 : 1;
            top.i_data = 0xAA;
            tick(&top);
            sink = top.o_data;
            push = !push;
        }
        auto t1 = std::chrono::high_resolution_clock::now();
        (void)sink;
        state.SetIterationTime(std::chrono::duration<double>(t1 - t0).count());
    }
}
BENCHMARK(BM_testbench_tick_x1000000)
    ->Name("testbench_tick_fifo_w8_d16_x1000000")
    ->UseManualTime()->Iterations(3)
    ->Unit(benchmark::kNanosecond);

BENCHMARK_MAIN();
