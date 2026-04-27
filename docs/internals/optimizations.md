# Simulator Optimization Algorithms

Celox applies optimizations across multiple layers from compile time to runtime to accelerate simulation. SIR optimization passes can be individually enabled/disabled via `OptimizeOptions`. The native x86-64 backend applies additional MIR-level optimizations automatically. The Cranelift backend optimization level is separately configurable via `CraneliftOptLevel`.

## 1. Logic Layer (SLT) Optimizations

These optimizations are applied during the stage where RTL logic expressions are analyzed.

### 1.1 Global Hash Consing
Expressions (`SLTNode`) with identical logical structure are deduplicated into shared instances across all modules and all `always_comb` blocks. This reduces memory usage and improves the efficiency of subsequent hoisting.

### 1.2 Topological Hoisting
Shared subexpressions referenced multiple times are moved forward in the instruction sequence so they are evaluated only once at the beginning of the simulation cycle. This significantly reduces redundant `Load` instructions and the number of operations.

## 2. Structural Layer (SIR) Optimizations

These are pass-based optimizations applied to the generated instruction sequence (SIR).

### 2.1 Load/Store Coalescing
-   **Load Coalescing**: Merges multiple `Load` operations to adjacent bit ranges at the same address into a single wider `Load`.
-   **Store Coalescing**: Combines writes to consecutive bit ranges at the same address using a `Concat` instruction, then executes them as a single `Store`.

### 2.2 Redundant Load Elimination (RLE / Forwarding)
Tracks values that have been loaded into registers or stored, and eliminates reloads to the same address by reusing the existing register value.

### 2.3 Commit Optimization
-   **Commit Sinking**: Pushes `Commit` instructions in merge blocks down into preceding `Store` instructions to combine them.
-   **Inline Forwarding**: Replaces generated `Commit` instructions with direct `Store` instructions where possible, eliminating unnecessary copies between buffers.

### 2.4 Dead Store Elimination
Detects and removes writes to the Working region that are never referenced.

### 2.5 Instruction Scheduling
Reorders instructions while preserving inter-instruction dependencies (RAW/WAR/WAW), taking into account processor execution ports and memory latency.

### 2.6 Scheduler-level Store Coalescing
During scheduling, consecutive DAG nodes targeting the same variable at the same topological layer are reordered to be adjacent. When contiguous bit ranges of the same variable are detected, they are merged into a single `Concat` + `Store` at the SIR level, reducing the number of memory writes. Controlled by `SirPass::CoalesceStores`.

### 2.7 Global Value Numbering (GVN)
Deduplicates SIR instructions with identical operands and eliminates dead code. Controlled by `SirPass::Gvn`.

### 2.8 Concat Folding
Folds redundant `Concat` operations — e.g., when a `Concat` reassembles slices of the same register in their original order, the Concat is eliminated. Controlled by `SirPass::ConcatFolding`.

### 2.9 XOR Chain Folding
Detects and folds XOR reduction chains into more efficient patterns. Controlled by `SirPass::XorChainFolding`.

### 2.10 Vectorize Concat
Vectorizes `Concat` patterns in combinational blocks — recognizes repeated similar operations across bit ranges and replaces them with wider operations. Controlled by `SirPass::VectorizeConcat`.

### 2.11 Split Coalesced Stores
Splits wide coalesced stores back into narrower ones after the reschedule pass, when the split form is more efficient for the backend. Controlled by `SirPass::SplitCoalescedStores`.

### 2.12 Partial Forward
Partial store-load forwarding in combinational blocks. When a store covers part of a subsequent load's range, forwards the known portion and narrows the load. Controlled by `SirPass::PartialForward`.

### 2.13 Identity Store Bypass
Detects identity copies (Store→Load roundtrips where the value is unchanged) and registers the source and destination as address aliases in `Program::address_aliases`. Aliased variables share physical memory, eliminating redundant copies. Controlled by `SirPass::IdentityStoreBypass`.

### 2.14 Tail-Call Splitting
Cranelift uses a 24-bit instruction index internally, limiting a single function to approximately 16M CLIF instructions. Large combinational designs (e.g., wide-bus arithmetic, many coalesced execution units) can exceed this limit.

When the estimated CLIF instruction count for `eval_comb` exceeds the threshold (currently 8M, a 50% safety margin), the optimizer splits it into a chain of smaller functions connected by Cranelift's `return_call` (tail-call) instruction, which avoids stack growth.

Three strategies are applied in order of increasing cost:

1.  **EU-boundary splitting**: Splits between execution units. Since `RegisterId`s are EU-scoped, no live registers need to be forwarded across the split boundary (zero overhead).
2.  **Intra-EU single-block splitting**: For a single-block EU that exceeds the threshold, splits at `Store` instruction boundaries. A dynamic programming pass minimizes the number of live registers that must be forwarded as tail-call arguments. A cost model (`cost_model.rs`) estimates per-instruction CLIF cost, calibrated against the actual translator (including quadratic costs for wide shifts, multiplication, and division).
3.  **Memory-spilled multi-block splitting**: For multi-block EUs (containing branches and loops), splits the CFG into chunks with a single-entry-point guarantee. Inter-chunk live registers are passed through a scratch memory region appended to the unified memory buffer, rather than as function arguments. Each chunk is compiled with signature `(mem_ptr) -> i64`, and cross-chunk edges emit spill stores followed by a tail-call.

This pass runs even when all SIR passes are disabled (`OptimizeOptions::none()`) to prevent compilation failures.

## Per-Pass Control

Each SIR optimization pass can be individually enabled or disabled via the `SirPass` enum and `OptimizeOptions`. `OptLevel::O0` enables only `TailCallSplit`; `OptLevel::O1` (default) and `O2` enable all 18 passes.

| `SirPass` variant | Pass(es) |
|---|---|
| `StoreLoadForwarding` | Load/Store Coalescing, Redundant Load Elimination (2.1, 2.2) |
| `HoistCommonBranchLoads` | Branch-shared load hoisting |
| `BitExtractPeephole` | `(value >> shift) & mask` → direct ranged load |
| `OptimizeBlocks` | Dead block removal, block merging |
| `SplitWideCommits` | Wide commit splitting |
| `CommitSinking` | Commit Sinking (2.3) |
| `InlineCommitForwarding` | Inline Forwarding (2.3) |
| `EliminateDeadWorkingStores` | Dead Store Elimination (2.4) |
| `Reschedule` | Instruction Scheduling (2.5) |
| `CoalesceStores` | Scheduler-level Store Coalescing (2.6) |
| `Gvn` | Global Value Numbering (2.7) |
| `ConcatFolding` | Concat Folding (2.8) |
| `XorChainFolding` | XOR Chain Folding (2.9) |
| `VectorizeConcat` | Vectorize Concat (2.10) |
| `SplitCoalescedStores` | Split Coalesced Stores (2.11) |
| `PartialForward` | Partial Forward (2.12) |
| `IdentityStoreBypass` | Identity Store Bypass (2.13) |
| `TailCallSplit` | Tail-Call Splitting (2.14) — enabled even at O0 |

## 3. Machine Layer (MIR) Optimizations — Native Backend

These optimizations are applied in the native x86-64 backend between instruction selection (ISel) and register allocation. They operate on MIR, a word-level SSA IR with virtual registers.

### Adaptive Pipeline

The MIR optimizer adapts its aggressiveness based on register pressure:

-   **High-pressure (VRegs > 40)**: Full pipeline with 2× iterative core passes, followed by load sinking and live range splitting for maximum optimization.
-   **Low-pressure (VRegs ≤ 40)**: Lightweight single-pass pipeline (skips load sinking and live range splitting).

### MIR Optimization Passes

| Pass | Description |
|---|---|
| Constant folding | Evaluate operations with constant operands at compile time |
| Constant deduplication | Merge duplicate `LoadImm` instructions |
| Copy propagation | Replace uses of `Mov` destinations with their sources |
| Algebraic simplification | Simplify patterns like `x & 0` → `0`, `x \| 0` → `x`, `x ^ 0` → `x`, strength-reduce `Mul` to shifts |
| Redundant mask elimination | Remove `AndImm` masks that are provably no-ops (uses known-width tracking) |
| Global value numbering (GVN) | Deduplicate instructions with identical operands (dominator-aware) |
| Dead code elimination (DCE) | Remove instructions whose results are unused |
| Lower to immediate forms | Convert `op reg, LoadImm(c)` → `opImm reg, c` |
| LoadImm sinking | Move constant loads closer to their uses to reduce register pressure (high-pressure only) |
| Live range splitting | Split long-lived values to improve register allocation (high-pressure only) |
| XOR chain to PEXT fusion | Fold XOR reduction chains into BMI2 `PEXT` instructions |
| If-conversion | Convert diamond `Branch` patterns into `Select` (cmov) for small arms |
| CFG simplification | Thread jumps through empty blocks, eliminate redundant branches |
| Value width computation | Track actual bit widths for narrowing optimizations in the emitter (e.g., 32-bit registers) |

## 4. Cranelift Backend Options

The Cranelift backend optimization level is separately configurable via `CraneliftOptLevel`:

| Level | Description |
|---|---|
| `None` | No Cranelift-level optimizations (skips egraph pass) |
| `Speed` (default) | Optimize for execution speed |
| `SpeedAndSize` | Optimize for both speed and code size |

Additional Cranelift backend options are available via `CraneliftOptions`:

| Option | Type | Default | Description |
|---|---|---|---|
| `regalloc_algorithm` | `Backtracking` / `SinglePass` | `Backtracking` | Register allocator algorithm. `SinglePass` is much faster but generates more spills |
| `enable_alias_analysis` | bool | `true` | Alias-aware redundant load optimization during egraph pass |
| `enable_verifier` | bool | `true` | Cranelift IR verifier — disable to save compile time |

For fastest compilation at the cost of simulation performance:
```rust
Simulator::builder(code, "Top")
    .cranelift_options(CraneliftOptions::fast_compile())
    .build()
```

This sets `opt_level = None`, `regalloc_algorithm = SinglePass`, disables alias analysis and the verifier.

## 5. Execution Layer (Behavioral) Optimizations

These are dynamic optimizations applied in the simulator's execution loop.

### 5.1 Silent Edge Skipping
When events such as clock signals occur but the signal value has not changed, or the trigger condition (rising/falling edge) is not met, evaluation of dependent flip-flops and re-evaluation of associated combinational logic are skipped.

### 5.2 Multi-Phase Evaluation (Separation of Evaluation and Update)
When multiple events are triggered simultaneously, all evaluations are performed based on the current values in the Stable region, followed by a bulk update. This guarantees consistent simulation results independent of the order in which events occur.
