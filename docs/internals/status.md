# Implementation Status Reference

This is an overview of the current implementation status and supported features of Celox.

## 1. Core Runtime

-   **JIT Compilation**: SIR-to-native-code translation via [Cranelift](https://cranelift.dev/).
-   **Memory Model**: Single-buffer management with separate Stable/Working regions.
-   **Scheduler**: Event-driven time management (BinaryHeap-based).
-   **Multi-clock Synchronization (Experimental)**: Supports per-clock-domain evaluation and automatic detection of cross-domain chaining (cascade clocks). *[Evaluation order and consistency are subject to limitations](./cascade-limitations.md).*
-   **Multi-phase Evaluation (In Development)**: An evaluation strategy to guarantee consistency for simultaneously occurring events. IR-level separation is implemented, but full application at the runtime level is ongoing.
-   **4-State Simulation**: X propagation via an IEEE 1800-compliant value/mask model. Normalization (`v &= ~m`) is applied across arithmetic, logic, shift, concatenation, Mux, and FF operations. See [four_state.md](./four-state.md) for details.

## 2. Language Features

### Combinational Circuits
-   **Basic Operations**: Arithmetic, logic, comparison, shift, concatenation, slicing.
-   **Control Structures**: Conditional branching with `if`, `case`, etc.
-   **Optimizations**: Common sub-expression hoisting at the SLT stage, Load/Store Coalescing at the SIR stage.

### Sequential Circuits
-   **Triggers**: `posedge`, `negedge` clocks, asynchronous reset (high/low active).
-   **Reset Synchronization**: Synchronous reset (`if_reset`) support.
-   **Data Paths**: Signal transfer across multiple clock domains (*assumes user-side synchronization design*).

## 3. Interfaces and Peripheral Features

-   **Interfaces**: Hierarchical connection resolution including `modport`.
-   **Waveform Output**: VCD format generation.
-   **SignalRef**: Fast signal access from external APIs.

## 4. Unimplemented Features and Future Work

-   **System Tasks**: Features requiring host-side callbacks, such as `$display` and `$finish`.
-   **Assertions**: Dynamic verification features such as `assert` and `assume`.
-   **Multithreading**: Parallel execution at the execution-unit or domain level.

## 5. Intentional Design Limitations

This simulator prioritizes efficient high-abstraction RTL verification. As such, the following features that depend on physical timing details are out of scope or have limited support.

-   **Gate-level Delays**: Delay specifications using `#` and inertial delay simulation.
-   **Post-layout Verification**: Timing back-annotation.
-   **Detailed Delta-cycle Behavior**: Reproducing tricky behaviors that depend on the standard Verilog event dispatch order.
