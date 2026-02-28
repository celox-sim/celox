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
        index: Vec<NodeId>,             // Dynamic index expressions (multi-dimensional)
        access: BitAccess,             // Bit range being referenced
    },

    // Constant
    Constant(BigUint, usize),           // (value, bit width)

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

    // Bit concatenation ({a, b} or reconstruction from partial assignments)
    Concat(Vec<(NodeId, usize)>),       // List of (expression reference, bit width)

    // Bit slice (e.g. v[7:0])
    Slice {
        expr: NodeId,
        access: BitAccess,
    },
}
```

### Dynamic Indexing in `Input` Nodes

A dynamic array access `arr[i][j]` is represented as follows:

```
Input {
    variable: VarId of arr,
    index: [NodeId(expression for i), NodeId(expression for j)],
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
    pub ranges: BTreeMap<usize, (T, usize)>,  // key: lsb, value: (value, width)
}
```

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
Initial state:  RangeStore: { 0: (None, 8) }

After y[3:0] = a:
  split_at(0), split_at(4)
  update([0,3], Some(Input(a)))
  RangeStore: { 0: (Some(Input(a)), 4), 4: (None, 4) }

After y[7:4] = b:
  update([4,7], Some(Input(b)))
  RangeStore: { 0: (Some(Input(a)), 4), 4: (Some(Input(b)), 4) }
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
4. **Kahn's algorithm**: Execute topological sort. Report a `CombinationalLoop` error if a cycle is found
5. **SIR generation**: Convert each `LogicPath`'s `expr(NodeId)` to SIR using `SLTToSIRLowerer` in sorted order

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
| `Mux` | Conditional branch via `Branch` terminator instruction |
| `Concat` | Lower each part -> combine with shift + OR |
| `Slice` | Lower expression -> shift + mask |

### Mux Lowering

`Mux` is converted into control flow:

```
Block_current:
    cond_reg = lower(cond)
    Branch { cond: cond_reg, true: (Block_then, []), false: (Block_else, []) }

Block_then:
    then_reg = lower(then_expr)
    Jump(Block_merge, [then_reg])

Block_else:
    else_reg = lower(else_expr)
    Jump(Block_merge, [else_reg])

Block_merge (params: [result_reg]):
    ... subsequent processing ...
```

This naturally achieves short-circuit evaluation (the expression in the unselected branch is not evaluated).

## Related Documents

- [Architecture Overview](./architecture.md) -- Overall simulator design
- [SIR Intermediate Representation Reference](./ir-reference.md) -- Detailed SIR instruction set (the lowering target)
- [Optimization Algorithms](./optimizations.md) -- Details on hash consing, hoisting, and more
