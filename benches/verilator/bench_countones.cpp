/**
 * Verilator benchmark harness for countones (W=64).
 * Mirrors Celox benchmark_countones. Uses Google Benchmark for timing.
 */

#include <benchmark/benchmark.h>
#include "VTop.h"
#include "verilated.h"
#include <chrono>
#include <cstdint>

// --- simulation_eval_countones_w64_x1 ---
static void BM_simulation_eval_x1(benchmark::State &state) {
    VTop top;
    uint64_t input = 0;
    for (uint64_t i = 0; i < 100000; i++) { top.i_data = i; top.eval(); }  // warm up
    for (auto _ : state) {
        top.i_data = input++;
        top.eval();
    }
}
BENCHMARK(BM_simulation_eval_x1)
    ->Name("simulation_eval_countones_w64_x1")
    ->Unit(benchmark::kNanosecond);

// --- simulation_eval_countones_w64_x1000000 ---
static void BM_simulation_eval_x1000000(benchmark::State &state) {
    VTop top;
    uint64_t input = 0;
    for (auto _ : state) {
        auto t0 = std::chrono::high_resolution_clock::now();
        for (int i = 0; i < 1000000; i++) { top.i_data = input++; top.eval(); }
        auto t1 = std::chrono::high_resolution_clock::now();
        state.SetIterationTime(std::chrono::duration<double>(t1 - t0).count());
    }
}
BENCHMARK(BM_simulation_eval_x1000000)
    ->Name("simulation_eval_countones_w64_x1000000")
    ->UseManualTime()->Iterations(3)
    ->Unit(benchmark::kNanosecond);

BENCHMARK_MAIN();
