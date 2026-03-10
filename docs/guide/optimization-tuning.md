# Optimization Tuning

Celox provides two layers of optimization control: **SIRT passes** (Celox's own IR optimizer) and **Cranelift backend options**. All optimizations are enabled by default. This guide explains how to tune them for your workload.

::: tip TL;DR
The default settings (all enabled) are the best general-purpose choice. Only tune if you have a specific compile-time or simulation-speed bottleneck, and always benchmark your actual design.
:::

## Quick Start

```ts
import { Simulator } from '@celox-sim/celox';

// Default: all optimizations enabled (best simulation speed)
const sim = await Simulator.create(module);

// Fast compilation mode (much slower simulation)
const sim = await Simulator.create(module, {
    craneliftOptLevel: "none",
    regallocAlgorithm: "singlePass",
    enableAliasAnalysis: false,
    enableVerifier: false,
});

// Disable all SIRT passes (Cranelift still optimizes)
const sim = await Simulator.create(module, { optimize: false });
```

## SIRT Optimization Passes

SIRT (Simulator IR Transform) passes optimize the intermediate representation before handing it to Cranelift for code generation.

| Pass | What it does |
|---|---|
| `storeLoadForwarding` | Reuses a stored value directly instead of reloading it from memory |
| `hoistCommonBranchLoads` | When both branches of a conditional start with the same load, moves it before the branch |
| `bitExtractPeephole` | Converts `(value >> shift) & mask` into a single ranged load |
| `optimizeBlocks` | Dead block removal, block merging |
| `splitWideCommits` | Splits wide commit operations into narrower ones |
| `commitSinking` | Moves commit operations closer to where their values are used |
| `inlineCommitForwarding` | Writes directly to the destination region, removing the intermediate commit copy |
| `eliminateDeadWorkingStores` | Removes stores to working memory that are never read |
| `reschedule` | Reorders instructions for better Cranelift code generation |

### Pass Interactions

The passes are **not independent**. They form a pipeline where earlier passes prepare the IR for later ones:

```
storeLoadForwarding ─┐
                     ├─► cleanIR ──► commitSinking ──► inlineCommitForwarding ──► ...
hoistCommonBranchLoads┘
```

`storeLoadForwarding` and `hoistCommonBranchLoads` simplify the IR so that `inlineCommitForwarding` can better match commit patterns. Disabling them individually may appear harmless, but **disabling them together** degrades the IR quality fed to Cranelift, causing compile time and simulation speed to suffer.

::: warning
Do not disable `storeLoadForwarding`, `hoistCommonBranchLoads`, and `inlineCommitForwarding` as a group. In benchmarks, this combination increased combinational compile time by +69% and eval time by +17%.
:::

### Critical Passes

These passes have the largest impact on simulation speed. Disabling them causes significant regressions:

| Pass | Sequential (tick) | Combinational (eval) |
|---|---|---|
| `reschedule` | **+322%** slower | +9% slower |
| `commitSinking` | **+207%** slower | +14% slower |
| `eliminateDeadWorkingStores` | **+163%** slower | +9% slower |
| `splitWideCommits` | **+161%** slower | +11% slower |
| `optimizeBlocks` | ~neutral | **+71%** slower |

### Design-Dependent Behavior

Sequential-heavy designs (many flip-flops, simple logic — e.g. 1000 parallel counters) and combinational-heavy designs (deep logic cones — e.g. SEC encoder/decoder) respond **oppositely** to the same tuning:

- Sequential designs have many commit operations → `commitSinking`, `splitWideCommits`, `eliminateDeadWorkingStores`, and `reschedule` are critical.
- Combinational designs have deep logic cones → `optimizeBlocks` is critical; commit-related passes have less effect.
- Some passes that slow down compilation for one design type speed it up for the other.

**There is no single non-default configuration that improves both design types.** Always benchmark your specific workload.

## Cranelift Backend Options

These control Cranelift's own code generation, separate from SIRT passes.

| Option | Default | Description |
|---|---|---|
| `craneliftOptLevel` | `"speed"` | `"none"` / `"speed"` / `"speedAndSize"` |
| `regallocAlgorithm` | `"backtracking"` | `"backtracking"` (better code) / `"singlePass"` (faster compile) |
| `enableAliasAnalysis` | `true` | Alias analysis in egraph pass |
| `enableVerifier` | `true` | IR correctness verifier |

### Impact by Design Type

| Option | Sequential (compile / tick) | Combinational (compile / eval) |
|---|---|---|
| `craneliftOptLevel: "none"` | −5% / −13% | **+27% / +123%** |
| `regallocAlgorithm: "singlePass"` | −16% / **+291%** | +33% / +31% |
| `enableAliasAnalysis: false` | −7% / −26% | +6% / +8% |
| `enableVerifier: false` | **−31%** / −26% | +6% / +12% |

Key takeaways:

- **`craneliftOptLevel: "none"`** helps sequential designs but **devastates combinational** (+123% eval time).
- **`regallocAlgorithm: "singlePass"`** saves compile time but simulation is **3-4x slower** for sequential designs.
- **`enableVerifier: false`** gives the best compile-time win for sequential designs (−31%) with acceptable simulation impact. For combinational designs the benefit is marginal.
- **`enableAliasAnalysis: false`** has minor effects in both directions.

## Benchmarking Your Design

A benchmark tool is included to measure the impact of each option on your designs:

```bash
cargo run --release --example pass_benchmark -p celox
```

This tests two representative designs (1000-counter sequential and SEC encoder/decoder combinational) with individual pass disabling, combination effects, and Cranelift options.

To test your own design, copy and modify the example, or use environment variables for quick profiling:

```bash
# Show per-phase timing (parse, optimize, JIT)
CELOX_PHASE_TIMING=1 cargo test my_test_name

# Show per-batch JIT compilation details
CELOX_PASS_TIMING=1 cargo test my_test_name
```

## Recommendations

| Goal | Configuration |
|---|---|
| Best simulation speed | Default (all enabled) |
| Fastest compilation | `craneliftOptLevel: "none"`, `enableVerifier: false` — but benchmark first |
| Rapid iteration (compile-time critical) | `optimize: false` + Cranelift defaults, or `fast_compile()` in Rust |
| Production simulation | Default — the compile cost pays for itself over many simulation cycles |
