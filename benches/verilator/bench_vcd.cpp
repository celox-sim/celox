// Same scalar counters and sampling points as crates/celox/benches/vcd.rs.
// The comparison runner generates VTop and vcd_fixture.h for each signal count.
#include "VTop.h"
#include "vcd_fixture.h"
#include "verilated.h"
#if VM_TRACE
#include "verilated_vcd_c.h"
#endif

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>

#if VM_TRACE
class CountingFile final : public VerilatedVcdFile {
public:
    uint64_t bytes = 0;
    ssize_t write(const char* data, ssize_t size) override {
        const auto written = VerilatedVcdFile::write(data, size);
        if (written > 0) bytes += static_cast<uint64_t>(written);
        return written;
    }
};
#endif

int main(int argc, char** argv) {
    try {
        if (argc != 5) throw std::runtime_error("expected MODE CASE STEPS OUTPUT");
        const std::string mode = argv[1];
        const std::string workload = argv[2];
        const uint64_t steps = std::stoull(argv[3]);
        const unsigned active = workload == "idle" ? 0
            : workload == "sparse" ? std::max(1U, kSignals / 100)
            : workload == "dense" ? kSignals
            : throw std::runtime_error("unknown workload");
#if VM_TRACE
        if (mode != "instrumented" && mode != "vcd")
            throw std::runtime_error("trace build expects instrumented or vcd");
#else
        if (mode != "off") throw std::runtime_error("untraced build expects off");
#endif
        auto context = std::make_unique<VerilatedContext>();
        context->threads(1);
#if VM_TRACE
        context->traceEverOn(true);
#endif
        auto top = std::make_unique<VTop>(context.get());
        top->clk = 0;
        top->rst = 0;
        set_enables(*top, 0);
        top->eval();
        top->clk = 1;
        top->eval();  // Assert the active-low reset on a clock edge.
        top->clk = 0;
        top->rst = 1;
        set_enables(*top, active);
        top->eval();
        top->clk = 1;
        top->eval();  // One untimed enabled tick, matching Celox's setup.
        top->clk = 0;
        top->eval();  // Celox's explicit tick leaves the clock signal at zero.

        uint64_t initial_bytes = 0;
        uint64_t bytes = 0;
#if VM_TRACE
        CountingFile sink;
        std::unique_ptr<VerilatedVcdC> trace;
        if (mode == "vcd") {
            trace = std::make_unique<VerilatedVcdC>(&sink);
            top->trace(trace.get(), 1);
            trace->open(argv[4]);
            if (!trace->isOpen()) throw std::runtime_error("cannot open trace output");
            trace->dump(uint64_t{0});
            trace->flush();
            initial_bytes = sink.bytes;
        }
#endif
        const auto start = std::chrono::steady_clock::now();
        for (uint64_t step = 1; step <= steps; ++step) {
            top->clk = 0;
            top->eval();
#if VM_TRACE
            if (trace) trace->dump(step * 2);
#endif
            top->clk = 1;
            top->eval();
#if VM_TRACE
            if (trace) trace->dump(step * 2 + 1);
#endif
        }
#if VM_TRACE
        if (trace) trace->flush();
#endif
        const auto elapsed = std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::steady_clock::now() - start).count();
#if VM_TRACE
        bytes = sink.bytes - initial_bytes;
#endif
        // Observe every counter after timing, including in the non-tracing build.
        // This also checks that the sparse enable configuration actually ran.
        verify_counters(*top, active, steps + 1);
        std::cout << "mode,case,signals,steps,elapsed_ns,bytes\n"
                  << mode << ',' << workload << ',' << kSignals << ',' << steps
                  << ',' << elapsed << ',' << bytes << '\n';
        top->final();
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
