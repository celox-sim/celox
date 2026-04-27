# Simulator Architecture

Celox is an engine that generates JIT-compiled native code from Veryl RTL and executes cycle-based simulation.

## Design Philosophy and Target

This simulator is designed with the goal of **maximizing verification efficiency for modern synchronous circuit designs (RTL)**.

-   **RTL-focused**: Physical timing reproduction that trades off against simulation speed -- such as gate-level delays (# delays) and detailed delta-cycle behavior -- is intentionally simplified by restricting the design scope to RTL-level logic verification.
-   **Performance-first**: Rather than interpreter-style emulation, the simulator compiles from SIR (Simulator IR) to native machine code to achieve execution throughput close to hand-written C.
-   **Consistency as a design goal**: Mechanisms such as "multi-phase evaluation" and "cascade clock detection" have been designed and implemented to guarantee consistency for challenges encountered in real RTL designs, such as multi-clock domains and zero-delay clock trees. However, there are currently [race condition limitations under certain conditions](./cascade-limitations.md).

## Compilation Pipeline

The transformation from Veryl source code to execution consists of the following three major phases.

1.  **Frontend (Parser/Analyzer)**:
    -   Parses Veryl source and generates the analyzer IR.
    -   `parser::parse_ir` takes this as input and converts each module into a `SimModule` (a struct containing SLT (logic expressions) and SIR (instruction sequences)).

2.  **Middle-end (Flattening/Scheduling/Optimization)**:
    -   **Flattening**: Flattens the instance hierarchy and converts module-local `VarId`s into global `AbsoluteAddr`s. Port connections are converted into `LogicPath`s.
    -   **Atomization**: Splits `LogicPath`s at bit boundaries (atoms) to analyze dependencies at bit-level precision.
    -   **Scheduling**: Topologically sorts the split atoms to determine the execution order of combinational logic. Detects SCCs via Tarjan's algorithm and handles cycles with static unrolling or dynamic convergence loops.
    -   **SIR Optimization**: Applies per-pass optimization (store-load forwarding, commit sinking, dead store elimination, instruction scheduling, etc.) controlled by `OptimizeOptions`.

3.  **Backend (Code Generation)**:
    -   **Memory Layout**: Determines memory offsets for all variables and places them on a single memory buffer with Stable, Working, Triggered-bits, and Scratch regions. Layout is pre-computed in `Program` after optimization, before backend codegen, so all backends share the same layout.
    -   **Code Generation**: Compiles SIR into executable machine code via one of the available backends.
    -   **Runtime**: Manages compiled function pointers as event handles and executes the simulation.
    -   **Testbench VM** (optional): A stack-based bytecode VM that executes Veryl `initial` blocks and testbench functions. Opcodes include `ConstU64`, `ConstWide`, `LoadU64`, `LoadWide`, `BinOp`, `UnaryOp`, `Ternary`, `LoadIndexed`, `LoadBitSelect`, `StoreU64`, supporting both narrow (≤64-bit) and wide signals.

## Backends

Celox supports multiple compilation backends, selected at build time based on the target architecture.

### Native x86-64 Backend (Default)

The self-hosted native backend is the default on x86-64 platforms. It compiles SIR through a dedicated pipeline:

```
SIR (bit-level)
  → ISel (Instruction Selection)
    → MIR (word-level SSA with VRegs)
      → mir_opt (MIR optimization passes)
        → regalloc (Braun & Hack MIN algorithm)
          → emit (x86-64 machine code via iced-x86)
```

Key features of the native backend:

-   **MIR**: A word-level SSA IR with virtual registers (`VReg`). Instructions operate on 64-bit values; bit-level access information is preserved in `SpillDesc` side-tables for cost-aware spill decisions.
-   **MIR Optimization**: Constant folding, copy propagation, algebraic simplification, GVN, DCE, if-conversion (Branch → Select/cmov), CFG simplification, PEXT fusion for XOR chains, and more. An adaptive pipeline runs the full pass set iteratively for high-pressure functions (VRegs > 40) and a lightweight variant for small functions.
-   **Register Allocator**: A unified single-pass allocator based on the Braun & Hack (2009) extended MIN algorithm. Performs simultaneous spilling and assignment in one forward pass with cost-aware eviction. Supports three spill kinds: SimState (reload from simulation memory), Stack, and Remat (rematerialize immediates).
-   **EU Merge**: Multiple execution units are merged into a single function with shared prologue/epilogue and `jmp`-linked boundaries, reducing call overhead.
-   **Cmp+Branch Fusion**: When a comparison result only feeds a branch, the `setcc`+`movzx`+`test` sequence is replaced by a direct `cmp`+`jcc`.

### Cranelift Backend (Fallback)

The Cranelift-based JIT backend (`JitBackend`) remains available for non-x86-64 targets and as a fallback. It compiles SIR directly to native code via [Cranelift](https://cranelift.dev/). Cranelift-specific options (`CraneliftOptLevel`, `RegallocAlgorithm`, `enable_alias_analysis`, `enable_verifier`) are configured through `CraneliftOptions`.

### WASM Backend

A WebAssembly backend (`wasm_codegen`) generates WASM bytecode from SIR. The Rust-side `WasmBackend` instantiates that bytecode via wasmtime, while the TypeScript playground path exposes the same generated bytes and runs them through the browser's WebAssembly APIs.

### Backend Trait

All backends implement the `SimBackend` trait, which provides a unified interface for:

-   Combinational evaluation (`eval_comb`)
-   Single-phase FF evaluation (`eval_apply_ff_at`) — fast path when a step can use combined evaluate+apply semantics
-   Split-phase FF evaluation (`eval_only_ff_at`, `apply_ff_at`) — for cascade clock consistency
-   Signal/event access (`resolve_signal`, `resolve_event`, `resolve_event_opt`, `resolve_eval_only_event`, `resolve_apply_event`)
-   Get/set operations (`get`, `set`, `set_wide`, `get_four_state`, `set_four_state`)
-   Memory/layout access (`memory_as_ptr`, `memory_as_mut_ptr`, `stable_region_size`, `layout`)
-   Triggered-bits management (`clear_triggered_bits`, `mark_triggered_bit`, `get_triggered_bits`)

## Memory Model

The simulator employs a **multi-region model on a single memory buffer**.

-   **Stable region**: Holds the current committed values. Combinational logic inputs and outputs reference this region.
-   **Working region**: Temporarily holds the next state of flip-flops. Only variables that are actually written have Working region slots allocated.
-   **Triggered-bits region**: One bit per event, used for cascade/gated clock trigger detection. After a `Store` instruction, the backend compares old and new values and sets the corresponding trigger bit if changed.
-   **Scratch region**: Used by the tail-call splitting pass for inter-chunk register value spilling.
-   **SignalRef**: A handle that caches offsets and metadata, enabling direct memory access without going through a `HashMap`.
-   **Address Aliases**: The `IdentityStoreBypass` optimization detects variables that are identity copies (Store→Load roundtrips) and registers them as aliases in `Program::address_aliases`. Aliased variables share physical memory, eliminating redundant copies.

For 4-state variables, each variable occupies `2 × ceil(width/8)` bytes (value + mask pair).

## Execution Control Logic

`Simulation::step` advances the simulation time by one step using the following flow.

1.  **Event extraction**: Retrieves all events occurring at the current time (such as clock changes) from the scheduler.
2.  **Clock edge detection**:
    -   Previous values are retained in a `BitSet` and compared with the updated values to determine `posedge` / `negedge`.
    -   Based on `DomainKind`, checks whether the target flip-flop groups have been triggered.
3.  **Silent edge skipping**: When a signal value has changed but the flip-flop trigger condition is not met (e.g., a falling edge when a rising edge is specified), unnecessary flip-flop evaluation is skipped.
4.  **Multi-phase evaluation**:
    -   When multiple domains are triggered simultaneously, to maintain consistency as an event-driven model, next-state computation via `eval_only` is first performed across all domains. Then, after all computations are complete, values are written to the Stable region all at once via `apply`. This avoids value inconsistencies between simultaneously occurring events.
5.  **Cascade clock detection**:
    -   To handle cases where a flip-flop output serves as the clock for another flip-flop (zero-delay clock tree), clock signal changes are re-scanned after domain evaluation, and evaluation is repeated until the state stabilizes.

## Related Components

-   **`SimBackend`**: Trait abstracting over compilation backends. `NativeBackend` (x86-64), `JitBackend` (Cranelift), and `WasmBackend` (wasmtime) implement this trait.
-   **`Scheduler`**: Manages events using a `BinaryHeap` and dispatches them in chronological order with deterministic ordering (time → event ID → signal).
-   **`VcdWriter`**: Records signal changes during simulation in VCD format.
-   **`MemoryLayout`**: Pre-computed offset map shared by all backends. Contains stable/working region offsets, variable widths, 4-state flags, triggered-bits region, and scratch region for inter-chunk spilling.
