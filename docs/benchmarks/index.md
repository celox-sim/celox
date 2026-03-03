# Benchmarks

Celox includes benchmark suites for the Rust core, the TypeScript runtime, and Verilator as a reference baseline. CI runs benchmarks on every push to `master` and publishes an interactive trend dashboard.

## Dashboard

<ClientOnly><BenchmarkDashboard /></ClientOnly>

You can also view the raw data on the [external dashboard](https://celox-sim.github.io/celox/dev/bench/).

## What is Measured

### Counter (N=1000)

The main workload uses a counter module (`Top`) with **N=1000** parallel 32-bit counter instances. This exercises the full JIT pipeline under a realistic workload. Rust, TypeScript, and Verilator all benchmark the same design for direct comparison.

| Benchmark | Rust | TS | Verilator |
|---|---|---|---|
| `simulation_build_top_n1000` | JIT compile | NAPI build | Verilate + C++ compile |
| `simulation_tick_top_n1000_x1` | Single tick | Single tick | Single tick |
| `simulation_tick_top_n1000_x1000000` | 1M ticks | 1M ticks | 1M ticks |
| `testbench_tick_top_n1000_x1` | Tick + read | Tick + read | Tick + read |
| `testbench_tick_top_n1000_x1000000` | 1M testbench cycles | 1M testbench cycles | 1M testbench cycles |
| `testbench_array_tick_top_n1000_x1` | — | Array `.at()` single | — |
| `testbench_array_tick_top_n1000_x1000000` | — | Array `.at()` 1M | — |

### Standard Library Modules

Benchmarks for Veryl stdlib modules across all three runtimes.

**Linear SEC (P=6)** — Hamming single-error-correcting encoder/decoder (57-bit data, 63-bit codeword). Combinational.

| Benchmark | Rust | TS | Verilator |
|---|---|---|---|
| `simulation_build_linear_sec_p6` | JIT compile | NAPI build | Verilate + C++ compile |
| `simulation_eval_linear_sec_p6_x1` | Single eval | Single eval | Single eval |
| `simulation_eval_linear_sec_p6_x1000000` | 1M evals | 1M evals | 1M evals |
| `testbench_eval_linear_sec_p6_x1000000` | 1M evals + read corrected | 1M evals + read corrected | 1M evals + read corrected |

**Countones (W=64)** — Recursive combinational popcount tree.

| Benchmark | Rust | TS | Verilator |
|---|---|---|---|
| `simulation_build_countones_w64` | JIT compile | NAPI build | Verilate + C++ compile |
| `simulation_eval_countones_w64_x1` | Single eval | Single eval | Single eval |
| `simulation_eval_countones_w64_x1000000` | 1M evals | 1M evals | 1M evals |

**std::counter (WIDTH=32)** — Multi-mode up/down counter with wrap-around.

| Benchmark | Rust | TS | Verilator |
|---|---|---|---|
| `simulation_build_std_counter_w32` | JIT compile | NAPI build | Verilate + C++ compile |
| `simulation_tick_std_counter_w32_x1` | Single tick | Single tick | Single tick |
| `simulation_tick_std_counter_w32_x1000000` | 1M ticks | 1M ticks | 1M ticks |
| `testbench_tick_std_counter_w32_x1000000` | 1M tick + read | 1M tick + read | 1M tick + read |

**std::gray_counter (WIDTH=32)** — Gray-encoded counter (counter + gray_encoder).

| Benchmark | Rust | TS | Verilator |
|---|---|---|---|
| `simulation_build_gray_counter_w32` | JIT compile | NAPI build | Verilate + C++ compile |
| `simulation_tick_gray_counter_w32_x1` | Single tick | Single tick | Single tick |
| `simulation_tick_gray_counter_w32_x1000000` | 1M ticks | 1M ticks | 1M ticks |
| `testbench_tick_gray_counter_w32_x1000000` | 1M tick + read | 1M tick + read | 1M tick + read |

### API & Overhead

| Benchmark | Description |
|---|---|
| `simulator_tick_x10000` | Raw Simulator::tick overhead (Rust & TS) |
| `simulation_step_x20000` | Simulation::step time-based API overhead (Rust & TS) |

## Running Locally

### Rust

```bash
cargo bench -p celox
```

### TypeScript

```bash
pnpm bench
```

This builds the NAPI addon in release mode, builds packages, then runs Vitest benchmarks.

### Verilator

```bash
bash scripts/run-verilator-bench.sh
```

Requires `verilator` and a C++ toolchain.

## CI Environment

Benchmarks run on GitHub Actions shared runners (`ubuntu-latest`). Because these runners share hardware with other workloads, some noise in the results is expected. The alert threshold is set to 200% to avoid false positives. Focus on long-term trends rather than individual data points.
