/**
 * Verilator benchmark harness for linear_sec encoder/decoder (P=6).
 * Mirrors Celox benchmark_linear_sec. Uses Google Benchmark for timing.
 */

#include <benchmark/benchmark.h>
#include "VLinearSecTop.h"
#include "verilated.h"
#include <chrono>
#include <cstdint>

// --- simulation_eval_linear_sec_p6_x1 ---
static void BM_simulation_eval_x1(benchmark::State &state) {
    VLinearSecTop top;
    uint64_t input = 0;
    for (uint64_t i = 0; i < 100000; i++) { top.i_word = i; top.eval(); }  // warm up
    for (auto _ : state) {
        top.i_word = input++;
        top.eval();
    }
}
BENCHMARK(BM_simulation_eval_x1)
    ->Name("simulation_eval_linear_sec_p6_x1")
    ->Unit(benchmark::kNanosecond);

// --- simulation_eval_linear_sec_p6_x1000000 ---
static void BM_simulation_eval_x1000000(benchmark::State &state) {
    VLinearSecTop top;
    uint64_t input = 0;
    for (auto _ : state) {
        auto t0 = std::chrono::high_resolution_clock::now();
        for (int i = 0; i < 1000000; i++) { top.i_word = input++; top.eval(); }
        auto t1 = std::chrono::high_resolution_clock::now();
        state.SetIterationTime(std::chrono::duration<double>(t1 - t0).count());
    }
}
BENCHMARK(BM_simulation_eval_x1000000)
    ->Name("simulation_eval_linear_sec_p6_x1000000")
    ->UseManualTime()->Iterations(3)
    ->Unit(benchmark::kNanosecond);

// --- testbench_eval_linear_sec_p6_x1000000 ---
static void BM_testbench_eval_x1000000(benchmark::State &state) {
    VLinearSecTop top;
    uint64_t input = 0;
    for (auto _ : state) {
        volatile uint8_t sink = 0;
        auto t0 = std::chrono::high_resolution_clock::now();
        for (int i = 0; i < 1000000; i++) {
            top.i_word = input++;
            top.eval();
            sink = top.o_corrected;
        }
        auto t1 = std::chrono::high_resolution_clock::now();
        (void)sink;
        state.SetIterationTime(std::chrono::duration<double>(t1 - t0).count());
    }
}
BENCHMARK(BM_testbench_eval_x1000000)
    ->Name("testbench_eval_linear_sec_p6_x1000000")
    ->UseManualTime()->Iterations(3)
    ->Unit(benchmark::kNanosecond);

BENCHMARK_MAIN();
