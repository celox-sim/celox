#!/bin/bash
# Build and run Verilator benchmarks.
# Outputs BENCH lines to stdout (build time + tick benchmarks).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_DIR="$SCRIPT_DIR/../benches/verilator"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cp "$BENCH_DIR/Top.sv" "$BENCH_DIR/bench_main.cpp" "$WORK/"
cd "$WORK"

# ── Measure build time (verilate + C++ compile) ──
BUILD_START=$(date +%s%N)
verilator --cc -O3 --exe bench_main.cpp Top.sv -CFLAGS "-O3"
make -C obj_dir -f VTop.mk -j"$(nproc)" OPT_FAST="-O3" >/dev/null 2>&1
BUILD_END=$(date +%s%N)
BUILD_NS=$(( BUILD_END - BUILD_START ))

echo "BENCH simulation_build_top_n1000 $BUILD_NS"

# ── Run tick / testbench benchmarks ──
./obj_dir/VTop
