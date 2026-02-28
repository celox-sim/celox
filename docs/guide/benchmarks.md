# Benchmarks

Celox includes benchmark suites for both the Rust core and the TypeScript runtime. CI runs benchmarks on every push to `master` and publishes an interactive trend dashboard.

## Dashboard

View the latest results and historical trends:

**[Benchmark Dashboard](/celox/dev/bench/)**

## What is Measured

All benchmarks use a counter module (`Top`) with **N=1000** parallel 32-bit counter instances. This exercises the full JIT pipeline under a realistic workload.

### Rust Benchmarks (Criterion)

| Benchmark | Description |
|---|---|
| `simulation_build_top_n1000` | JIT compile time |
| `simulation_tick_top_n1000_x1` | Single clock tick |
| `simulation_tick_top_n1000_x1000000` | 1M ticks in a loop |
| `testbench_tick_top_n1000_x1` | Single testbench cycle (write + tick + read) |
| `testbench_tick_top_n1000_x1000000` | 1M testbench cycles |
| `simulator_tick_x10000` | Raw Simulator::tick, 10k iterations |
| `simulation_step_x20000` | Simulation::step, 20k steps |

### TypeScript Benchmarks (Vitest)

| Benchmark | Description |
|---|---|
| `simulation_build_top_n1000` | JS build / JIT compile time |
| `simulation_tick_top_n1000_x1` | Single tick |
| `simulation_tick_top_n1000_x1000000` | 1M ticks in a loop |
| `testbench_tick_top_n1000_x1` | Single testbench cycle |
| `testbench_tick_top_n1000_x1000000` | 1M testbench cycles |
| `testbench_array_tick_top_n1000_x1` | Single cycle with array `.at()` access |
| `testbench_array_tick_top_n1000_x1000000` | 1M cycles with array `.at()` access |

The Rust and TypeScript benchmarks mirror each other, so you can directly compare the performance of the two runtimes.

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

## CI Environment

Benchmarks run on GitHub Actions shared runners (`ubuntu-latest`). Because these runners share hardware with other workloads, some noise in the results is expected. The alert threshold is set to 200% to avoid false positives. Focus on long-term trends rather than individual data points.
