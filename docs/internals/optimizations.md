# Simulator Optimization Algorithms

Celox applies optimizations across multiple layers from compile time to runtime to accelerate simulation.

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

## 3. Execution Layer (Behavioral) Optimizations

These are dynamic optimizations applied in the simulator's execution loop.

### 3.1 Silent Edge Skipping
When events such as clock signals occur but the signal value has not changed, or the trigger condition (rising/falling edge) is not met, evaluation of dependent flip-flops and re-evaluation of associated combinational logic are skipped.

### 3.2 Multi-Phase Evaluation (Separation of Evaluation and Update)
When multiple events are triggered simultaneously, all evaluations are performed based on the current values in the Stable region, followed by a bulk update. This guarantees consistent simulation results independent of the order in which events occur.
