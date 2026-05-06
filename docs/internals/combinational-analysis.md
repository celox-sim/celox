# SLT Combinational Circuit Analysis Guide

## Overview

This document explains how the simulator analyzes combinational circuits (`always_comb` blocks)
and transforms them into executable instruction sequences.

Combinational circuit processing follows this pipeline:

```
always_comb block (veryl_analyzer::ir)
    |
    v  Symbolic evaluation (comb.rs)
LogicPath<VarId>  --  NodeId references + source dependency info
    |
    v  Flattening (flatting.rs)
LogicPath<AbsoluteAddr>
    |
    v  atomize (flatting.rs)
LogicPath<AbsoluteAddr>  --  Split along bit boundaries
    |
    +  CombObserver / RuntimeEventSite metadata for always_comb side effects
    |
    v  Topological sort + lowering (scheduler.rs + lower.rs)
ExecutionUnit<AbsoluteAddr>  --  SIR instruction sequence
```

## SLTNode (Symbolic Logic Tree)

`SLTNode<A>` is a tree structure that represents expressions in combinational circuits.
In the current implementation, nodes are stored in an `SLTNodeArena` and expressions are referenced by `NodeId`.

```rust
pub enum SLTNode<A> {
    // Reference to an input variable
    Input {
        variable: A,                    // Variable address
        index: Vec<SLTIndex>,           // Dynamic index expressions (multi-dimensional)
        access: BitAccess,             // Bit range being referenced
    },

    // Constant (4-state aware)
    Constant(BigUint, BigUint, usize, bool),  // (value, mask, bit width, is_4state)

    // Binary operation
    Binary(NodeId, BinaryOp, NodeId),

    // Unary operation
    Unary(UnaryOp, NodeId),

    // Conditional select (generated from if statements)
    Mux {
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
    },

    // Runtime for-loop fold. This represents a loop as a state transition
    // instead of unrolling every iteration at parse time.
    ForFold {
        loop_var: A,
        start: SLTLoopBound,
        end: SLTLoopBound,
        result: VarAtomBase<A>,
        initials: Vec<SLTForUpdate<A>>,
        updates: Vec<SLTForUpdate<A>>,
        effects: Vec<SLTForEffect>,
        continue_cond: NodeId,
        // loop width, signedness, inclusivity, step, and direction omitted here
    },

    // Bit concatenation ({a, b} or reconstruction from partial assignments)
    Concat(Vec<(NodeId, usize)>),       // List of (expression reference, bit width)

    // Bit slice (e.g. v[7:0])
    Slice {
        expr: NodeId,
        access: BitAccess,
    },
}
```

### `SLTIndex` -- Dynamic Index with Stride

```rust
pub struct SLTIndex {
    pub node: NodeId,    // Expression computing the index value
    pub stride: usize,   // Byte stride per index step (element size)
}
```

### `ForFold` -- Runtime Loop Fold

`ForFold` is used when an `always_comb` `for` loop must stay as a runtime loop. This happens for dynamic bounds and for loops whose body contains runtime side effects.

The node models one loop as a fold over a small symbolic state:

- `initials`: values of each loop-carried target before the first iteration
- `updates`: next-state expressions produced by one symbolic iteration
- `result`: the target bit range whose final value this `ForFold` node returns
- `continue_cond`: a predicate that keeps assignments disabled after a `break`
- `effects`: side effects to emit inside each runtime iteration

This is intentionally different from syntactic unrolling. The parser symbolically evaluates the loop body once with the loop variable represented as an input-like value, extracts the changed ranges as `updates`, and lowers the fold into SIR control flow later.

### Dynamic Indexing in `Input` Nodes

A dynamic array access `arr[i][j]` is represented as follows:

```
Input {
    variable: VarId of arr,
    index: [SLTIndex { node: expr_i, stride: elem_size }, SLTIndex { node: expr_j, stride: ... }],
    access: BitAccess { lsb: 0, msb: element_width - 1 },
}
```

When `index` is empty, the access is static, and the bit position is determined solely by `access`.

## LogicPath -- Data Path Representation

`LogicPath` represents a single data path in a combinational circuit.
It describes "which bit range of which variable is determined by which expression, depending on which inputs."

```rust
pub struct LogicPath<A> {
    pub target: VarAtomBase<A>,              // Write destination (variable + bit range)
    pub sources: HashSet<VarAtomBase<A>>,     // Set of read sources
    pub expr: NodeId,                         // Reference to the expression tree that computes the value
}
```

### `VarAtomBase` -- Variable Reference with Bit Range

```rust
pub struct VarAtomBase<A> {
    pub id: A,              // Variable address
    pub access: BitAccess,  // Bit range [lsb, msb]
}
```

### Example

```systemverilog
always_comb {
    y = a + b;
}
```

This produces the following `LogicPath`:

```
LogicPath {
    target: VarAtom { id: y, access: [0, width-1] },
    expr: n42,  // e.g. node ID in the Arena
    sources: { VarAtom(a, [0, width-1]), VarAtom(b, [0, width-1]) },
}
```

## Symbolic Evaluation Algorithm

### Entry Point: `parse_comb`

`parse_comb` takes a `CombDeclaration` (an `always_comb` block) and returns
a `CombResult` (a list of `LogicPath`s and a bit boundary map).

```
parse_comb(module, decl) -> CombResult { paths, boundaries }
```

### SymbolicStore -- Symbolic State

`SymbolicStore` is the data structure that manages the current symbolic value of each variable.

```rust
pub type SymbolicStore<A> =
    HashMap<VarId, RangeStore<Option<(NodeId, HashSet<VarAtomBase<A>>)>>>;
```

Breaking down the structure:

- Outer `HashMap<VarId, ...>`: per-variable entries
- `RangeStore<...>`: manages expressions per bit range (described below)
- `Option<...>`: `None` = unmodified, `Some` = assigned
- `(NodeId, HashSet<VarAtomBase>)`: a pair of (expression tree reference, source dependency set)

In the initial state, all variables are initialized to `None` (unmodified).
Each time an assignment statement is evaluated, the corresponding bit range of the target variable is updated to `Some(...)`.

### RangeStore -- Bit Range Management

`RangeStore<T>` is an interval map that manages values per bit range.

```rust
pub struct RangeStore<T> {
    pub ranges: BTreeMap<usize, (T, usize, usize)>,  // key: lsb, value: (value, width, origin_lsb)
}
```

The third element `origin_lsb` tracks the original bit position when this data was first placed, which is preserved even when the range is split.

Key operations:

| Method | Description |
|---|---|
| `new(initial, width)` | Initialize the entire bit range with `initial` |
| `split_at(bit)` | Split a range at the specified bit position |
| `update(access, value)` | Update the value for a specified bit range |
| `get_parts(access)` | Retrieve all parts within a specified range |

This allows partial assignments to be tracked precisely.

#### Example: Tracking Partial Assignments

```systemverilog
logic [7:0] y;
always_comb {
    y[3:0] = a;
    y[7:4] = b;
}
```

```
Initial state:  RangeStore: { 0: (None, 8, 0) }

After y[3:0] = a:
  split_at(0), split_at(4)
  update([0,3], Some(Input(a)))
  RangeStore: { 0: (Some(Input(a)), 4, 0), 4: (None, 4, 4) }

After y[7:4] = b:
  update([4,7], Some(Input(b)))
  RangeStore: { 0: (Some(Input(a)), 4, 0), 4: (Some(Input(b)), 4, 4) }
```

### Statement Evaluation

#### `eval_assign` -- Assignment Statement

Handles assignments with static indices. Symbolically evaluates the RHS expression and writes the result to the `SymbolicStore`.

```
eval_assign(module, store, boundaries, stmt)
  -> (updated_store, updated_boundaries)
```

1. Evaluate the RHS expression with `eval_expression` -> `(NodeId, sources)`
2. Compute the bit range of the LHS
3. Update the symbolic state with `store[lhs_var].update(access, Some((expr, sources)))`

#### `eval_dynamic_assign` -- Dynamic Index Assignment

Handles assignments to dynamic indices such as `arr[i] = value`.
Since the write destination bit position can only be determined at runtime for dynamic indices,
a `LogicPath` covering the entire bit range of the variable is generated immediately.

#### `eval_if` -- Conditional Statement

Evaluates each branch of an `if` statement independently and merges the results using `Mux` nodes.

```
eval_if(module, store, boundaries, stmt)
```

1. Evaluate the condition expression -> `cond_node`
2. Evaluate the then branch with a clone of `store` -> `then_store`
3. Evaluate the else branch with a clone of `store` -> `else_store`
4. Merge the results of `then_store` and `else_store` for each variable using `Mux`

**Important**: When there is no `else` clause, unassigned bit ranges remain as `None` (unmodified).
In the final stage, `None` parts are restored as `Input` (a reference to the current value of the variable itself).
This corresponds to latch inference in combinational circuits.

#### Read-Before-Write in `always_comb`

When an `always_comb` block reads a variable before assigning to that same variable, the read observes the value from before the procedural block execution. This can disagree with synthesis-oriented intuition, but it is the behavior that follows from reading the LRM procedural ordering directly. Celox tracks these reads as `previous_sources` on the generated `LogicPath`.

After flattening and atomization, `previous_sources` are removed from normal dataflow dependencies and converted into ordering edges. This prevents the scheduler from moving the later write before the earlier read. Identity aliasing is also blocked for addresses that are loaded as snapshots in the same evaluation, because sharing storage would turn the required previous-value read into a read of the later write.

#### `eval_for` -- Runtime Fold for `always_comb` Loops

`always_comb` `for` loops are not always expanded into repeated statements. Constant forward loops can be walked directly while collecting side effects, but the data-path evaluator still uses `eval_for_with_effects` as the common representation for dynamic loops, reverse loops, stepped loops, and loops with runtime side effects.

The algorithm is:

1. Validate the loop variable width and bounds. An exclusive upper bound one past the loop type's maximum is allowed for full-range loops such as `0..256` with an 8-bit loop variable; other out-of-range bounds are rejected.
2. Collect every destination range written by the loop body, including writes through statement-form function output arguments.
3. Build a loop-local store:
   - written ranges start as `None` because they are produced by the loop
   - untouched ranges are copied from the pre-loop store so partial writes preserve their old symbolic value
   - the loop variable is inserted as an unknown symbolic value
4. Evaluate the loop body once as a state transition. Assignments, nested `if`s, nested loops, and statement-form function calls update the loop-local store.
5. Extract changed ranges from the one-iteration store diff. These become `SLTForUpdate` records.
6. Allocate one `ForFold` node per changed target range. The final store for each target is updated with that `ForFold` expression.

The loop variable and loop-carried variables are removed from the dependency source set of the final `ForFold`. Dependencies come from the loop bounds, initial values, non-loop-carried inputs used by the update expressions, and the `continue_cond`.

`break` is represented by `continue_cond`. After a `break` path is taken, subsequent statements in the same symbolic loop iteration are merged against the previous store, so they do not keep updating state after the logical loop has stopped.

When a loop contains side effects but has no data-path updates, the evaluator creates a dummy loop-carried update for the loop variable. This gives the lowerer a concrete `ForFold` node to execute, even though the only externally visible behavior is the effect emission.

## `always_comb` Runtime Effects

Most `always_comb` statements can be represented as pure symbolic expressions. Runtime effects such as `$display`, `$write`, `$assert`, and `$assert_continue` cannot: they must run at the observation point required by `always_comb` sensitivity, and their arguments must be captured at the correct statement position.

Celox handles these statements through a side-channel:

```rust
struct CombObserver<A> {
    site_id: u32,
    guard: Option<NodeId>,
    args: Vec<NodeId>,
    loop_runner: Option<NodeId>,
    sensitivity: Vec<VarAtomBase<A>>,
    local_inputs: Vec<(A, NodeId)>,
    observed_inputs: Vec<VarAtomBase<A>>,
    position_inputs: Vec<VarAtomBase<A>>,
    preceding_writes: Vec<VarAtomBase<A>>,
    written_before: Vec<VarAtomBase<A>>,
    written_inputs: Vec<A>,
    captured_in_loop: bool,
}
```

`CombEffectCollector` walks the same statements as the data-path evaluator, but records runtime event sites and observers instead of producing assignments. For each effect it:

1. Creates a `RuntimeEventSite` with the event kind, optional format template, argument widths, signedness, and string flags.
2. Evaluates the effect arguments into `NodeId`s using the current `SymbolicStore`.
3. Adds argument and guard sources to the observer sensitivity set.
4. Captures local symbolic values for variables that were assigned earlier in the same `always_comb` and are read by the effect.
5. Records statement-position dependencies (`preceding_writes`, `written_before`, and `position_inputs`) so the scheduler can place the capture after the writes whose values the effect must see.

The sensitivity computation follows the `always_comb` rule that expressions written in the block are excluded from the implicit sensitivity list. For dynamic accesses, Celox excludes only the statically known written prefix and keeps the remaining possible read atoms sensitive.

Effect emission is split from observer activation. Stores that can change an observer's sensitive inputs lower as:

```text
old = Load(target)
Store(target, new)
CombCaptureEnableIfChanged(old, new, sites)
```

The eventual `CombCaptureEvent` consumes the enabled site and writes a runtime event record with the captured arguments. Keeping activation as a separate instruction is important because store coalescing and address aliasing must not erase the old-value comparison needed to decide whether an observer should run.

### Effects Inside `for`

Effects inside dynamic `always_comb` loops are the tricky case. A single static observer is not enough because `$display` or assert arguments may depend on the loop variable and on loop-carried state for each runtime iteration.

For these loops, `collect_dynamic_for_effects` performs a second, effect-only symbolic pass over one loop iteration:

1. It builds the same loop-local store as `eval_for_with_effects`.
2. It temporarily enables `collector.loop_effects`.
3. Runtime effects encountered in the loop body are recorded as `SLTForEffect` entries instead of only as top-level observers.
4. `eval_for_with_effects` receives those `SLTForEffect`s and embeds them in the loop's `ForFold`.
5. The first observer captured in the loop receives `loop_runner = Some(for_fold_node)`, so the scheduler has a path that executes the loop even when the loop only produces effects.

During lowering, `SLTToSIRLowerer::lower_for_effects` runs inside the generated loop body. It lowers each effect argument with an environment that binds the current loop variable and loop-carried state registers, then emits `CombCaptureEvent`.

Guard handling is encoded per effect:

- `$display` / `$write` emit when the active branch guard is true.
- `$assert` / `$assert_continue` emit when their assertion condition fails, combined with any active branch guard.
- `$assert` with fatal severity stores a synthetic fatal error code in the emitted event.

### Bit Boundary Collection

`BoundaryMap<A>` holds the set of bit boundaries for each variable.

```rust
pub type BoundaryMap<A> = HashMap<A, BTreeSet<usize>>;
```

Boundaries are collected automatically during expression evaluation. When a bit slice `v[7:4]` of a variable is referenced,
bit positions `4` and `8` are added to the boundary set of `v`.

### Final LogicPath Generation

In the final stage of `parse_comb`, `LogicPath`s are generated from the `SymbolicStore`:

1. Retrieve the `Some(...)` parts (i.e., assigned ranges) from each variable's `RangeStore`
2. Exclude identity transformations (assignments to `Input(self)`)
3. Generate a `LogicPath` for each remaining part

### `combine_parts` -- Merging Parts

`combine_parts` merges multiple bit range parts into a single expression.

```rust
combine_parts(parts: Vec<((NodeId, sources), BitAccess)>) -> (NodeId, sources)
```

- If there is only one part: return it as-is
- If there are multiple parts: combine them with a `Concat` node

`combine_parts_with_default` is used when `None` (unmodified) parts are present,
inserting `Input` (a reference to the current value) in place of `None` entries.

## Atomize -- Splitting Along Bit Boundaries

After flattening, when integrating `LogicPath`s from multiple modules,
different modules may reference different bit ranges of the same variable.

`atomize_logic_paths` splits each `LogicPath` into minimal bit units (atoms) based on the boundary map.
This enables the scheduler to build precise dependency relationships.

```
atomize_logic_paths(paths, boundaries) -> atomized_paths
```

The `BitAccess` of each `LogicPath`'s target and sources is split at the boundaries,
and `Slice` nodes are inserted as needed.

## Scheduling

`scheduler::sort` topologically sorts all `LogicPath`s and produces `ExecutionUnit`s.

### Algorithm

1. **Build spatial index**: Map which bit range of each variable is driven by which `LogicPath`
2. **Detect multiple drivers**: Report an error if multiple paths drive the same bit range
3. **Build dependency graph**: Inspect whether each path's sources overlap with another path's targets, and add edges accordingly
4. **SCC extraction (Tarjan)**: Detect strongly connected components. SCCs with more than one node (or a self-loop) represent combinational cycles
5. **Layer computation + DAG reordering**: Compute topological layers and reorder consecutive DAG SCCs by `(layer, target_id)` so that paths targeting the same variable at the same layer become adjacent, enabling store coalescing
6. **SIR generation**: Process each SCC with one of two strategies:
   - **Strategy A (Static Unrolling)**: For small DAG parts or loops with predictable convergence bounds (total ops ≤ 32). The SCC is unrolled a fixed number of times based on structural dependency depth.
   - **Strategy B (Dynamic Convergence)**: For complex SCCs or user-annotated True Loops. Emits a runtime convergence loop with a dirty flag and safety limit. Each iteration checks whether values have changed; if all signals stabilize, the loop exits early. If the safety limit is exceeded, a `DetectedTrueLoop` runtime error is raised.

### Store Coalescing in Scheduling

When multiple DAG nodes at the same topological layer target consecutive bit ranges of the same variable, `flush_pending_coalesce` merges them into a single `Concat` + `Store`. Requirements: contiguous bit ranges, within the variable's declared width, and no self-references. This optimization is skipped in 4-state mode.

### Cycle Handling

Cycles in the dependency graph can be:
- **Ignored loops**: Edges explicitly marked as non-problematic by the user (false loops)
- **True loops**: Edges annotated with a user-specified safety limit for dynamic convergence
- **Unauthorized cycles**: Reported as `CombinationalLoop` errors

The FAS (Feedback Arc Set) sort algorithm (`greedy_fas_sort`) determines the optimal evaluation order within an SCC by maximizing forward edges and minimizing back-edges.

### Errors

```rust
pub enum SchedulerError<A> {
    CombinationalLoop { blocks: Vec<LogicPath<A>> },
    MultipleDriver { blocks: Vec<LogicPath<A>> },
}
```

## SLT -> SIR Lowering

`SLTToSIRLowerer` recursively converts `SLTNode`s into SIR instruction sequences.

Key conversion rules:

| SLTNode | SIR |
|---|---|
| `Input` | `Load` instruction (includes offset calculation when dynamic indices are present) |
| `Constant` | `Imm` instruction |
| `Binary` | Recursively lower left and right -> `Binary` instruction |
| `Unary` | Recursively lower operand -> `Unary` instruction |
| `Mux` | Lower both arms and emit the dedicated `Mux` SIR instruction |
| `ForFold` | Emit SIR loop control flow, loop-carried state registers, optional `CombCaptureEvent`s, and return the requested final state |
| `Concat` | Lower each part and emit the dedicated `Concat` SIR instruction |
| `Slice` | Lower expression -> `Slice` instruction |

### Mux Lowering

Expression-level `Mux` is now lowered to a data-select instruction:

```text
cond_reg = lower(cond)
then_reg = lower(then_expr)
else_reg = lower(else_expr)
result = Mux(cond_reg, then_reg, else_reg)
```

The dedicated instruction is important for 4-state simulation because it selects the exact value/mask pair of the chosen branch and preserves Z bits. Control-flow branches still exist in SIR for loops and dynamic convergence handling.

### `ForFold` Lowering

`ForFold` lowering creates explicit SIR blocks for loop header, body, and exit. The loop counter is cast to the declared loop variable width before body expressions are lowered. Each loop-carried update target is assigned a state register that is threaded through the loop header and updated at the end of the body.

When `effects` is non-empty, they are lowered before the next-state update expressions in the body. This preserves statement order for side effects that observe values produced earlier in the loop iteration. The same `ForFold` node can therefore both compute a folded data-path result and emit per-iteration runtime events.

## Related Documents

- [Architecture Overview](./architecture.md) -- Overall simulator design
- [SIR Intermediate Representation Reference](./ir-reference.md) -- Detailed SIR instruction set (the lowering target)
- [Optimization Algorithms](./optimizations.md) -- Details on hash consing, hoisting, and more
