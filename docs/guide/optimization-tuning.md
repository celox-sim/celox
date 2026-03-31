# Optimization Tuning

Celox provides two layers of optimization control: **SIRT passes** (Celox's own SIR-level optimizer) and **backend options**. On x86-64 the native backend is used by default; on other architectures Cranelift JIT is the fallback.

::: tip TL;DR
The default settings (all enabled, native backend on x86-64) are the best general-purpose choice. Only tune if you have a specific compile-time or simulation-speed bottleneck, and always benchmark your actual design.
:::

## Quick Start

```ts
import { Simulator } from '@celox-sim/celox';

// Default: all optimizations enabled (best simulation speed)
const sim = await Simulator.create(module);

// Disable all SIRT passes (backend still optimizes)
const sim = await Simulator.create(module, { optimize: false });

// Fine-grained: disable specific passes
const sim = await Simulator.create(module, {
    optimizeOptions: { reschedule: false, commitSinking: false },
});
```

## SIRT Optimization Passes

SIRT (Simulator IR Transform) passes optimize the intermediate representation before handing it to the backend for code generation.

| Pass | What it does |
|---|---|
| `storeLoadForwarding` | Reuses a stored value directly instead of reloading it from memory |
| `hoistCommonBranchLoads` | When both branches of a conditional start with the same load, moves it before the branch |
| `bitExtractPeephole` | Converts `(value >> shift) & mask` into a single ranged load |
| `optimizeBlocks` | Dead block removal, block merging, load coalescing |
| `splitWideCommits` | Splits wide commit operations into narrower ones |
| `commitSinking` | Moves commit operations closer to where their values are used |
| `inlineCommitForwarding` | Writes directly to the destination region, removing the intermediate commit copy |
| `eliminateDeadWorkingStores` | Removes stores to working memory that are never read |
| `reschedule` | Reorders instructions to reduce register pressure |

### Post-Merge Passes (Native Backend)

When multiple execution units are merged at the SIR level, additional passes run on the merged EU:

| Pass | What it does |
|---|---|
| Working memory elimination | Redirects WORKING region accesses to STABLE for independent variables |
| Cross-EU commit forwarding | Forwards stored values across EU boundaries, eliminating redundant commits |
| Coalesced store splitting | Breaks wide Concat+Store back into 64-bit stores interleaved with computation, reducing register pressure |

### MIR Optimization Passes (Native Backend)

The native backend has its own MIR-level optimization pipeline that runs after ISel:

| Pass | What it does |
|---|---|
| Constant folding | Evaluates operations with constant operands at compile time |
| Algebraic simplification | Identity, annihilation, strength reduction (e.g. `mul x, 2^n` → `shl x, n`) |
| Redundant mask elimination | Removes AND masks when known-bits analysis proves they are unnecessary |
| Global value numbering (GVN) | Dominator-tree scoped CSE with alias-aware Load invalidation |
| If-conversion | Converts diamond-shaped branches into Select (cmov) for small arms |
| Cmp+Branch fusion | Emits `cmp + jcc` directly instead of `setcc + movzx + test + jne` |
| 32-bit emit | Uses 32-bit registers when values are known ≤ 32 bits (auto zero-extend) |
| Branch fall-through | Eliminates `jmp` when the target is the next block in layout order |
| CFG simplification | Threads jumps through empty blocks |

### Pass Interactions

The passes are **not independent**. They form a pipeline where earlier passes prepare the IR for later ones:

```
storeLoadForwarding ─┐
                     ├─► cleanIR ──► commitSinking ──► inlineCommitForwarding ──► ...
hoistCommonBranchLoads┘
```

`storeLoadForwarding` and `hoistCommonBranchLoads` simplify the IR so that `inlineCommitForwarding` can better match commit patterns. Disabling them individually may appear harmless, but **disabling them together** degrades the IR quality fed to the backend, causing compile time and simulation speed to suffer.

::: warning
Do not disable `storeLoadForwarding`, `hoistCommonBranchLoads`, and `inlineCommitForwarding` as a group. In benchmarks, this combination increased combinational compile time by +69% and eval time by +17%.
:::

## Backend Selection

| Platform | Default Backend | Notes |
|---|---|---|
| x86-64 | **Native** | Custom x86-64 codegen, fastest |
| ARM / RISC-V | Cranelift JIT | Automatic fallback |
| WASM | WASM codegen | For Playground |

### Cranelift Backend Options

These apply only when using the Cranelift backend (non-x86-64 platforms, or explicit `build_cranelift()`):

| Option | Default | Description |
|---|---|---|
| `craneliftOptLevel` | `"speed"` | `"none"` / `"speed"` / `"speedAndSize"` |
| `regallocAlgorithm` | `"backtracking"` | `"backtracking"` (better code) / `"singlePass"` (faster compile) |
| `enableAliasAnalysis` | `true` | Alias analysis in egraph pass |
| `enableVerifier` | `true` | IR correctness verifier |

### Rust API

```rust
use celox::{Simulator, OptimizeOptions};

// Default (native on x86-64):
let sim = Simulator::builder(code, "Top").build()?;

// Explicit Cranelift backend:
let sim = Simulator::builder(code, "Top").build_cranelift()?;

// Per-pass control:
let sim = Simulator::builder(code, "Top")
    .optimize_options(OptimizeOptions { reschedule: false, ..OptimizeOptions::all() })
    .build()?;

// Disable all SIRT passes:
let sim = Simulator::builder(code, "Top")
    .optimize(false)
    .build()?;
```

### TypeScript API

```ts
// Default: all optimizations, native backend on x86-64
const sim = await Simulator.create(module);

// Fine-grained:
const sim = await Simulator.create(module, {
    optimizeOptions: { reschedule: false, commitSinking: false },
});

// Disable all SIRT passes:
const sim = await Simulator.create(module, { optimize: false });
```
