/**
 * Verilator benchmark harness for onehot (W=64).
 * Mirrors Celox benchmark_onehot. Combinational only.
 */

#include <benchmark/benchmark.h>
#include "VTop.h"
#include "verilated.h"
#include <chrono>
#include <cstdint>

// --- simulation_eval_onehot_w64_x1 ---
static void BM_eval_x1(benchmark::State &state) {
    VTop top;
    uint64_t input = 0;
    for (uint64_t i = 0; i < 100000; i++) { top.i_data = i; top.eval(); }
    for (auto _ : state) {
        top.i_data = input++;
        top.eval();
        benchmark::DoNotOptimize(top.o_onehot);
    }
}
BENCHMARK(BM_eval_x1)
    ->Name("simulation_eval_onehot_w64_x1")
    ->Unit(benchmark::kNanosecond);

// --- simulation_eval_onehot_w64_x1000000 ---
static void BM_eval_x1000000(benchmark::State &state) {
    VTop top;
    uint64_t input = 0;
    for (auto _ : state) {
        volatile uint8_t sink = 0;
        auto t0 = std::chrono::high_resolution_clock::now();
        for (int i = 0; i < 1000000; i++) {
            top.i_data = input++;
            top.eval();
            sink = top.o_onehot;
        }
        auto t1 = std::chrono::high_resolution_clock::now();
        (void)sink;
        state.SetIterationTime(std::chrono::duration<double>(t1 - t0).count());
    }
}
BENCHMARK(BM_eval_x1000000)
    ->Name("simulation_eval_onehot_w64_x1000000")
    ->UseManualTime()->Iterations(3)
    ->Unit(benchmark::kNanosecond);

BENCHMARK_MAIN();
