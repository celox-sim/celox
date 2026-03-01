/**
 * Verilator benchmark harness – mirrors the Celox Criterion benchmarks.
 *
 * Output format (one line per benchmark):
 *   BENCH <name> <nanoseconds>
 */

#include "VTop.h"
#include "verilated.h"
#include <chrono>
#include <cstdint>
#include <cstdio>

using Clock = std::chrono::high_resolution_clock;

static void reset(VTop *top) {
    top->rst = 1;
    top->clk = 0;
    top->eval();
    top->clk = 1;
    top->eval();
    top->rst = 0;
    top->clk = 0;
    top->eval();
}

static inline void tick(VTop *top) {
    top->clk = !top->clk;
    top->eval();
}

int main(int argc, char **argv) {
    Verilated::commandArgs(argc, argv);

    // --- simulation_tick_top_n1000_x1 ---
    {
        VTop *top = new VTop;
        reset(top);

        // Warm up
        for (int i = 0; i < 10000; i++)
            tick(top);

        const int ITERS = 100000;
        auto start = Clock::now();
        for (int i = 0; i < ITERS; i++)
            tick(top);
        auto end = Clock::now();

        double ns =
            std::chrono::duration<double, std::nano>(end - start).count() /
            ITERS;
        std::printf("BENCH simulation_tick_top_n1000_x1 %.2f\n", ns);
        delete top;
    }

    // --- simulation_tick_top_n1000_x1000000 ---
    {
        VTop *top = new VTop;
        reset(top);

        const int TICKS = 1000000;
        auto start = Clock::now();
        for (int i = 0; i < TICKS; i++)
            tick(top);
        auto end = Clock::now();

        double ns =
            std::chrono::duration<double, std::nano>(end - start).count();
        std::printf("BENCH simulation_tick_top_n1000_x1000000 %.2f\n", ns);
        delete top;
    }

    // --- testbench_tick_top_n1000_x1 (tick + read output) ---
    {
        VTop *top = new VTop;
        reset(top);

        for (int i = 0; i < 10000; i++)
            tick(top);

        volatile uint32_t sink = 0;
        const int ITERS = 100000;
        auto start = Clock::now();
        for (int i = 0; i < ITERS; i++) {
            tick(top);
            sink = top->cnt[0];
        }
        auto end = Clock::now();

        double ns =
            std::chrono::duration<double, std::nano>(end - start).count() /
            ITERS;
        std::printf("BENCH testbench_tick_top_n1000_x1 %.2f\n", ns);
        (void)sink;
        delete top;
    }

    // --- testbench_tick_top_n1000_x1000000 ---
    {
        VTop *top = new VTop;
        reset(top);

        volatile uint32_t sink = 0;
        const int TICKS = 1000000;
        auto start = Clock::now();
        for (int i = 0; i < TICKS; i++) {
            tick(top);
            sink = top->cnt[0];
        }
        auto end = Clock::now();

        double ns =
            std::chrono::duration<double, std::nano>(end - start).count();
        std::printf("BENCH testbench_tick_top_n1000_x1000000 %.2f\n", ns);
        (void)sink;
        delete top;
    }

    return 0;
}
