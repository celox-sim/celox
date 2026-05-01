# CLAUDE.md

## Project Overview

Celox is a JIT simulator for Veryl HDL. It compiles Veryl designs with Cranelift for high-speed simulation. Future plans include SystemVerilog/Verilog support.

## Build Commands

```bash
cargo build              # Build all crates
cargo test               # Run all tests
cargo test -p celox      # Run tests for the core crate only
cargo run -p celox-ts-gen -- --help  # TypeScript type generator CLI
```

### Snapshot Tests

```bash
cargo insta test         # Run snapshot tests
cargo insta accept       # Accept snapshot changes
```

## Workspace Structure

| Crate / Package | Description |
|---|---|
| `crates/celox` | Core simulator (IR, JIT compilation, runtime) |
| `crates/celox-macros` | Procedural macros |
| `crates/celox-napi` | N-API bindings for Node.js |
| `crates/celox-ts-gen` | CLI tool for TypeScript type generation |
| `crates/celox-bench-sv` | SystemVerilog generator for Verilator benchmarks |
| `packages/celox` | TypeScript runtime package |
| `packages/vite-plugin` | Vite plugin |

## Veryl Dependencies

The workspace depends on released Veryl crates: `veryl-analyzer`, `veryl-emitter`, `veryl-parser`, `veryl-metadata`, `veryl-path`, and `veryl-simulator`.

- `default-features = false` is set on `veryl-parser` to suppress parser regeneration during builds.
- After updating Veryl crate versions, run `cargo test` to verify compatibility.

### Analyzer API

The Veryl analyzer pass functions (`analyze_pass1`, `analyze_post_pass1`, `analyze_pass2`, `analyze_post_pass2`) return `Vec<AnalyzerError>`. All 4 passes must be called and their errors checked. `SimulatorError::Analyzer` wraps these errors.

### Writing Veryl in Tests

The analyzer enforces strict checks. When writing Veryl source in integration tests:

- **Clock domain annotations**: Multi-clock designs require `'a`/`'b` (or `'_` for single-clock) on all ports and vars. Cross-domain access needs `unsafe (cdc) { ... }`.
- **Logical operators on multi-bit**: `a && b` / `a || b` / `!a` are rejected for operands wider than 1 bit. Use reduction: `(|a) && (|b)`, `!(|a)`.
- **logic → bit assignment**: Requires explicit cast `as u8`.
- **SV keywords as identifiers**: Forbidden (e.g. `reg`). Use alternatives like `r_val`.
- **Clock from logic**: A `var` of type `logic` cannot be used as a clock. Use an external `clock` input or `let gated: '_ clock = clk_input & en;` (first operand must be clock-typed).
- **Self-referential assign**: `assign v = f(v);` is rejected as `UnassignVariable`. Use `always_comb` with `if`/`else` branches if possible, or redesign the circuit.

## Optimizer Options

GCC-style optimization level presets with per-pass overrides.

### OptLevel

`OptLevel` (`crates/celox/src/optimizer.rs`) sets defaults for SIR passes, Cranelift backend, and DSE:

| Level | SIR Passes | DSE | Cranelift |
|---|---|---|---|
| `O0` | TailCallSplit only | Off | `fast_compile()` |
| `O1` (default) | All 18 passes | Off | Speed / Backtracking |
| `O2` | All 18 passes | PreserveTopPorts | Speed / Backtracking |

### SirPass

`SirPass` enum (`crates/celox/src/optimizer.rs`) — all 18 SIR optimization passes:

| Pass | Description |
|---|---|
| `StoreLoadForwarding` | Propagates stored values to subsequent loads |
| `HoistCommonBranchLoads` | Hoists loads shared across all branches to the entry |
| `BitExtractPeephole` | Converts `(value >> shift) & mask` → direct ranged loads |
| `OptimizeBlocks` | General block-level optimizations (dead block removal, merging) |
| `SplitWideCommits` | Splits wide commit operations into narrower ones |
| `CommitSinking` | Sinks commit operations closer to their use site |
| `InlineCommitForwarding` | Inlines values forwarded through commit operations |
| `EliminateDeadWorkingStores` | Removes working-memory stores that are never read |
| `Reschedule` | Reorders instructions for better backend codegen |
| `CoalesceStores` | Merges consecutive narrow stores into wider Concat+Store |
| `Gvn` | Global value numbering / dead code elimination |
| `ConcatFolding` | Folds redundant Concat operations |
| `XorChainFolding` | Folds XOR chains |
| `VectorizeConcat` | Vectorizes Concat patterns in combinational blocks |
| `SplitCoalescedStores` | Splits wide coalesced stores back after reschedule |
| `PartialForward` | Partial store-load forwarding in combinational blocks |
| `IdentityStoreBypass` | Detects identity copies and registers address aliases |
| `TailCallSplit` | Splits large functions into tail-call chains (enabled even at O0) |

### Cranelift Backend Optimization

`CraneliftOptions` (`crates/celox/src/optimizer.rs`) provides fine-grained Cranelift backend control:

| Field | Type | Default | Description |
|---|---|---|---|
| `opt_level` | `CraneliftOptLevel` | `Speed` | Optimization level (`None` / `Speed` / `SpeedAndSize`) |
| `regalloc_algorithm` | `RegallocAlgorithm` | `Backtracking` | Register allocator (`Backtracking` = better code / `SinglePass` = faster compile) |
| `enable_alias_analysis` | `bool` | `true` | Alias analysis in egraph pass (only effective when `opt_level` ≠ `None`) |
| `enable_verifier` | `bool` | `true` | Cranelift IR verifier (disable to save compile time) |

`CraneliftOptions::fast_compile()` is a preset: `opt_level=None`, `regalloc=SinglePass`, alias analysis off, verifier off.

### Rust API

```rust
use celox::{OptLevel, SirPass, OptimizeOptions, CraneliftOptions, RegallocAlgorithm};

// OptLevel preset:
Simulator::builder(code, "Top")
    .opt_level(OptLevel::O2)
    .build()

// OptLevel + per-pass override:
Simulator::builder(code, "Top")
    .opt_level(OptLevel::O1)
    .disable_pass(SirPass::Reschedule)
    .enable_pass(SirPass::Gvn)
    .build()

// Shorthand (true=O1, false=O0):
Simulator::builder(code, "Top")
    .optimize(false)
    .build()

// Fine-grained Cranelift options:
Simulator::builder(code, "Top")
    .cranelift_options(CraneliftOptions::fast_compile())
    .build()

// Individual Cranelift toggles:
Simulator::builder(code, "Top")
    .regalloc_algorithm(RegallocAlgorithm::SinglePass)
    .enable_alias_analysis(false)
    .enable_verifier(false)
    .build()
```

### NAPI / TypeScript

```ts
// New API: OptLevel + pass overrides
const sim = await Simulator.create(module, {
    optLevel: "O2",
    passOverrides: ["-sir:reschedule", "+sir:gvn"],
});

// Legacy (still supported, deprecated):
const sim2 = await Simulator.create(module, { optimize: false });
const sim3 = await Simulator.create(module, {
    optimizeOptions: { reschedule: false },
});
```

- **NAPI**: `opt_level: "O0"|"O1"|"O2"`, `pass_overrides: ["+sir:<pass>", "-sir:<pass>"]` (`crates/celox-napi/src/lib.rs`)
- **TS**: `optLevel: "O0"|"O1"|"O2"`, `passOverrides: string[]` (`packages/celox/src/types.ts`)
- **Legacy**: `optimize: bool`, `optimizeOptions: OptimizeOptions` — still accepted, overridden by new fields.

## Dead Store Elimination (DSE)

DSE removes stores that are never read, improving JIT performance. Controlled via `DeadStorePolicy` (`crates/celox/src/simulator/builder.rs`):

| Policy | Behavior |
|---|---|
| `Off` (default) | No elimination — all stores preserved |
| `PreserveTopPorts` | Eliminate except top-module port stores |
| `PreserveAllPorts` | Eliminate except port stores of **all** instances |

### Rust API

```rust
// Explicit DSE policy:
Simulator::builder(code, "Top")
    .dead_store_policy(DeadStorePolicy::PreserveAllPorts)
    .build()

// Via OptLevel (O2 defaults to PreserveTopPorts):
Simulator::builder(code, "Top")
    .opt_level(OptLevel::O2)
    .build()
```

### NAPI / TypeScript

- **NAPI option**: `dead_store_policy: "off" | "preserve_top_ports" | "preserve_all_ports"` (`crates/celox-napi/src/lib.rs`)
- **TS option**: `deadStorePolicy: "off" | "preserveTopPorts" | "preserveAllPorts"` in `SimulatorOptions` (`packages/celox/src/types.ts`)
- **Mapping**: `buildNapiOpts()` in `napi-helpers.ts` converts camelCase → snake_case

### Hierarchy Filtering

DSE-enabled DUTs must not expose sub-instance signals that were eliminated. `filterHierarchyForDse()` (`napi-helpers.ts`) adjusts the hierarchy passed to `createDut()`:
- `preserveTopPorts` → `children = {}` (sub-instances stripped)
- `preserveAllPorts` → hierarchy intact (all instance ports survive)

### Vite Plugin `?dse=` Query

Import with `?dse=preserveAllPorts` or `?dse=preserveTopPorts` to bake `defaultOptions.deadStorePolicy` into the `ModuleDefinition`. `?dse` (no value) defaults to `preserveAllPorts`.

```ts
import { Adder } from './Adder.veryl?dse=preserveAllPorts';
```

`Simulator.create()` / `Simulation.create()` merge `module.defaultOptions` with caller-supplied options (caller wins).

## Rust Edition

This project uses Rust **edition 2024**.

## GitHub Project

- **URL**: https://github.com/orgs/celox-sim/projects/1
- **Owner**: celox-sim
- **Project number**: 1

タスク管理・ロードマップ管理に使用する。Issue を作成・更新する際は Project に紐づけること。

### フィールド

| フィールド | 種類 | 値 |
|---|---|---|
| Status | Single Select | `Backlog` / `In Progress` / `In Review` / `Done` |
| Priority | Single Select | `P0 Critical` / `P1 High` / `P2 Medium` / `P3 Low` |
| Milestone | 標準 | フェーズ・リリース単位で管理 |

### Issue 操作例

```bash
# Issue を Project に追加
gh project item-add 1 --owner celox-sim --url <issue-url>

# フィールド更新
gh project item-edit --project-id PVT_kwDOD8WmI84BQmif --id <item-id> --field-id <field-id> --single-select-option-id <option-id>
```
