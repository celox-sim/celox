# Implementation Status Reference

This is an overview of the current implementation status and supported features of Celox.

## 1. Core Runtime

-   **Native x86-64 Backend (Default)**: Self-hosted compilation pipeline (SIR → ISel → MIR → mir_opt → regalloc → x86-64 emit). Default backend on x86-64 platforms.
-   **Cranelift JIT Backend (Fallback)**: SIR-to-native-code translation via [Cranelift](https://cranelift.dev/). Available for non-x86-64 targets.
-   **WASM Backend**: SIR-to-WASM compilation for browser-based simulation (Playground).
-   **Memory Model**: Single-buffer management with Stable, Working, Triggered-bits, and Scratch regions.
-   **Scheduler**: Event-driven time management (BinaryHeap-based) with deterministic ordering.
-   **Multi-clock Synchronization**: Supports per-clock-domain evaluation and automatic detection of cross-domain chaining (cascade clocks). *[Evaluation order and consistency are subject to limitations](./cascade-limitations.md).*
-   **Multi-phase Evaluation**: An evaluation strategy to guarantee consistency for simultaneously occurring events. Separate `eval_only` / `apply` execution units enable split-phase flip-flop processing.
-   **4-State Simulation**: X propagation via an IEEE 1800-compliant value/mask model. The distinction between Z (`v=0, m=1`) and X (`v=1, m=1`) is preserved throughout the pipeline — no normalization is applied. See [four_state.md](./four-state.md) for details.

## 2. Language Features

### Combinational Circuits
-   **Basic Operations**: Arithmetic, logic, comparison, shift, concatenation, slicing.
-   **Control Structures**: Conditional branching with `if`, `case`, etc.
-   **Optimizations**: Common sub-expression hoisting at the SLT stage, Load/Store Coalescing and scheduler-level store coalescing at the SIR stage.

### Sequential Circuits
-   **Triggers**: `posedge`, `negedge` clocks, asynchronous reset (high/low active).
-   **Reset Synchronization**: Synchronous reset (`if_reset`) support.
-   **Data Paths**: Signal transfer across multiple clock domains (*assumes user-side synchronization design*).

## 3. Interfaces and Peripheral Features

-   **Interfaces**: Hierarchical connection resolution including `modport`.
-   **Waveform Output**: VCD format generation.
-   **SignalRef**: Fast signal access from external APIs.
-   **Child Instance Access**: Read/write sub-module ports via `child_signal()` / `instance_signals()` / `named_hierarchy()`. TypeScript DUT accessors support nested access (`dut.u_sub.o_data`).
-   **Parameter Overrides**: Numeric parameters can be overridden at runtime via `SimulatorOptions.parameters`. Type parameters are not supported at runtime (use wrapper modules via `celox.toml` `[test] sources`).

## 4. Unimplemented Features and Future Work

-   **System Tasks**: Features requiring host-side callbacks, such as `$display` and `$finish`.
-   **Assertions**: Dynamic verification features such as `assert` and `assume`.
-   **Multithreading**: Parallel execution at the execution-unit or domain level.

## 5. Intentional Design Limitations

This simulator prioritizes efficient high-abstraction RTL verification. As such, the following features that depend on physical timing details are out of scope or have limited support.

-   **Gate-level Delays**: Delay specifications using `#` and inertial delay simulation.
-   **Post-layout Verification**: Timing back-annotation.
-   **Detailed Delta-cycle Behavior**: Reproducing tricky behaviors that depend on the standard Verilog event dispatch order.
