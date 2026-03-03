#!/bin/bash
# Build and run Verilator benchmarks.
# Outputs: BENCH lines (build time) followed by Google Benchmark JSON blocks.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_DIR="$SCRIPT_DIR/../benches/verilator"

run_bench() {
    local build_name="$1" top="$2" sv="$3" cpp="$4"
    local work; work="$(mktemp -d)"

    cp "$BENCH_DIR/$sv" "$BENCH_DIR/$cpp" "$work/"

    local t0; t0=$(date +%s%N)
    (
        cd "$work"
        verilator --cc -O3 --exe "$cpp" "$sv" --top-module "$top" \
            -CFLAGS "-O3" -LDFLAGS "-lbenchmark -lpthread"
        make -C obj_dir -f "V${top}.mk" -j"$(nproc)" OPT_FAST="-O3" >/dev/null 2>&1
    )
    local t1; t1=$(date +%s%N)

    echo "BENCH $build_name $((t1 - t0))"
    "$work/obj_dir/V${top}" --benchmark_format=json --benchmark_color=false 2>/dev/null
    rm -rf "$work"
}

# ── Regenerate SV from Veryl stdlib sources ──
echo "Generating SV from Veryl stdlib..."
cargo run -p celox-bench-sv --release 2>/dev/null

# ── Counter benchmark (N=1000) ──
run_bench simulation_build_top_n1000      Top          Top.sv       bench_main.cpp

# ── LinearSec benchmark (P=6: 57-bit data, 63-bit codeword) ──
run_bench simulation_build_linear_sec_p6  LinearSecTop LinearSec.sv bench_linear_sec.cpp

# ── Countones benchmark (W=64) ──
run_bench simulation_build_countones_w64  Top          Countones.sv bench_countones.cpp

# ── std::counter benchmark (WIDTH=32) ──
run_bench simulation_build_std_counter_w32  Top        StdCounter.sv bench_std_counter.cpp

# ── std::gray_counter benchmark (WIDTH=32) ──
run_bench simulation_build_gray_counter_w32 Top        GrayCounter.sv bench_gray_counter.cpp
