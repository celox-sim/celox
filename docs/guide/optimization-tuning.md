# Optimization Tuning

Celox provides two layers of optimization control: **SIRT passes** (Celox's own SIR-level optimizer) and **backend options**. On x86-64 the native backend is used by default; on other architectures Cranelift JIT is the fallback.

::: tip TL;DR
The default settings (`O1`, native backend on x86-64) are the best general-purpose choice. Only tune if you have a specific compile-time or simulation-speed bottleneck, and always benchmark your actual design.
:::

## Optimization Levels

Celox uses a GCC-style optimization model: **preset levels** set defaults, and **per-pass overrides** allow fine-grained control.

| Level | SIR Passes | DSE | Cranelift |
|---|---|---|---|
| `O0` | TailCallSplit only | Off | `fast_compile()` |
| `O1` (default) | All 18 passes | Off | Speed / Backtracking |
| `O2` | All 18 passes | PreserveTopPorts | Speed / Backtracking |

## Quick Start

```ts
import { Simulator } from '@celox-sim/celox';

// Default (O1): all optimizations enabled
const sim = Simulator.create(module);

// O0: minimal optimization (fast compile, slower simulation)
const fastCompile = Simulator.create(module, { optLevel: "O0" });

// O2: all optimizations + dead store elimination
const simO2 = Simulator.create(module, { optLevel: "O2" });

// O1 with specific passes disabled
const custom = Simulator.create(module, {
    optLevel: "O1",
    passOverrides: ["-sir:reschedule", "-sir:commit_sinking"],
});

// Legacy (still supported):
const legacy = Simulator.create(module, { optimize: false });
```

## SIRT Optimization Passes

SIRT (Simulator IR Transform) passes optimize the intermediate representation before handing it to the backend for code generation. All 18 passes are individually controllable via `SirPass` (Rust) or `passOverrides` (TypeScript).

| Pass | What it does |
|---|---|
| `store_load_forwarding` | Reuses a stored value directly instead of reloading it from memory |
| `hoist_common_branch_loads` | When both branches of a conditional start with the same load, moves it before the branch |
| `bit_extract_peephole` | Converts `(value >> shift) & mask` into a single ranged load |
| `optimize_blocks` | Dead block removal, block merging, load coalescing |
| `split_wide_commits` | Splits wide commit operations into narrower ones |
| `commit_sinking` | Moves commit operations closer to where their values are used |
| `inline_commit_forwarding` | Writes directly to the destination region, removing the intermediate commit copy |
| `eliminate_dead_working_stores` | Removes stores to working memory that are never read |
| `reschedule` | Reorders instructions to reduce register pressure |
| `coalesce_stores` | Merges consecutive narrow stores into wider Concat+Store operations |
| `gvn` | Global value numbering / dead code elimination |
| `concat_folding` | Folds redundant Concat operations |
| `xor_chain_folding` | Folds XOR chains |
| `vectorize_concat` | Vectorizes Concat patterns in combinational blocks |
| `split_coalesced_stores` | Splits wide coalesced stores back after reschedule to reduce register pressure |
| `partial_forward` | Partial store-load forwarding in combinational blocks |
| `identity_store_bypass` | Detects identity copies and registers address aliases for layout sharing |
| `tail_call_split` | Splits large functions into tail-call chains (enabled even at O0) |

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
Do not disable `store_load_forwarding`, `hoist_common_branch_loads`, and `inline_commit_forwarding` as a group. In benchmarks, this combination increased combinational compile time by +69% and eval time by +17%.
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
use celox::{Simulator, OptLevel, SirPass, CraneliftOptions};

// Default (O1, native on x86-64):
let sim = Simulator::builder(code, "Top").build()?;

// O0 (fast compile):
let sim = Simulator::builder(code, "Top")
    .opt_level(OptLevel::O0)
    .build()?;

// O1 with specific pass disabled:
let sim = Simulator::builder(code, "Top")
    .opt_level(OptLevel::O1)
    .disable_pass(SirPass::Reschedule)
    .build()?;

// Explicit Cranelift backend:
let sim = Simulator::builder(code, "Top").build_cranelift()?;
```

### TypeScript API

```ts
// Default: O1, native backend on x86-64
const sim = Simulator.create(module);

// O2: all optimizations + DSE
const simO2 = Simulator.create(module, { optLevel: "O2" });

// O1 with per-pass overrides:
const custom = Simulator.create(module, {
    optLevel: "O1",
    passOverrides: ["-sir:reschedule", "-sir:commit_sinking"],
});

// O0 (fast compile, slower simulation):
const fastCompile = Simulator.create(module, { optLevel: "O0" });
```
