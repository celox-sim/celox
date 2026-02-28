# Simulator Architecture

`veryl-simulator` is an engine that generates JIT-compiled native code from Veryl RTL and executes cycle-based simulation.

## Design Philosophy and Target

This simulator is designed with the goal of **maximizing verification efficiency for modern synchronous circuit designs (RTL)**.

-   **RTL-focused**: Physical timing reproduction that trades off against simulation speed -- such as gate-level delays (# delays) and detailed delta-cycle behavior -- is intentionally simplified by restricting the design scope to RTL-level logic verification.
-   **Performance-first**: Rather than interpreter-style emulation, the simulator JIT-compiles from SIR (Simulator IR) to achieve execution throughput close to native code.
-   **Consistency as a design goal**: Mechanisms such as "multi-phase evaluation" and "cascade clock detection" have been designed and implemented to guarantee consistency for challenges encountered in real RTL designs, such as multi-clock domains and zero-delay clock trees. However, there are currently [race condition limitations under certain conditions](./cascade-limitations.md).

## Compilation Pipeline

The transformation from Veryl source code to execution consists of the following three major phases.

1.  **Frontend (Parser/Analyzer)**:
    -   Parses Veryl source and generates the analyzer IR.
    -   `parser::parse_ir` takes this as input and converts each module into a `SimModule` (a struct containing SLT (logic expressions) and SIR (instruction sequences)).

2.  **Middle-end (Flattening/Scheduling)**:
    -   **Flattening**: Flattens the instance hierarchy and converts module-local `VarId`s into global `AbsoluteAddr`s. Port connections are converted into `LogicPath`s.
    -   **Atomization**: Splits `LogicPath`s at bit boundaries (atoms) to analyze dependencies at bit-level precision.
    -   **Scheduling**: Topologically sorts the split atoms to determine the execution order of combinational logic.

3.  **Backend (JIT Compilation)**:
    -   **Memory Layout**: Determines memory offsets for all variables and places them on a single memory buffer.
    -   **JIT Engine**: Uses [Cranelift](https://cranelift.dev/) to compile SIR into native machine code.
    -   **Runtime**: Manages compiled function pointers as `EventRef`s and executes the simulation.

## Memory Model

The simulator employs a **two-region model on a single memory buffer**.

-   **Stable region**: Holds the current committed values. Combinational logic inputs and outputs reference this region.
-   **Working region**: Temporarily holds the next state of flip-flops.
-   **SignalRef**: A handle that caches offsets and metadata, enabling direct memory access without going through a `HashMap`.

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

-   **`JitBackend`**: Holds compiled function pointers (`SimFunc`) and invokes them directly through `EventRef`.
-   **`Scheduler`**: Manages events using a `BinaryHeap` and dispatches them in chronological order.
-   **`VcdWriter`**: Records signal changes during simulation in VCD format.
