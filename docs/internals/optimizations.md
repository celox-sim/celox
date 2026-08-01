# Optimization Architecture

Celox optimizes at three compiler layers and once more in the runtime. Each layer
uses the representation that can prove its transformations without depending on
details from adjacent phases.

```text
SLT structure
    ▼
SIR control flow and state accesses
    ▼
backend-private machine IR
    ▼
runtime event scheduling
```

User-facing optimization presets and trade-offs are documented in
[Optimization Tuning](/guide/optimization-tuning). This page describes where
the mechanisms live.

## Symbolic logic layer

SLT optimizations operate while RTL expressions and bit ranges are still
explicit.

- **Hash consing** interns structurally identical expressions so common logic has
  one symbolic identity.
- **Topological hoisting** materializes shared subexpressions once at a legal
  dependency point.
- **Range atomization** splits state at observed bit boundaries, avoiding
  whole-value work when only a slice is required.
- **Cost-directed mux lowering** chooses between eager selection and control flow
  before expression structure is lost.

This layer may use design dependency facts, but it does not know physical memory
offsets or target instructions.

## SIR layer

`celox-sir-opt` owns backend-independent transformations over execution units and
their control-flow graphs. The main families are:

| Family | Purpose |
|---|---|
| Forwarding and coalescing | Reuse known state values and combine adjacent bit-range accesses |
| CFG simplification | Fold proven branches, remove unreachable blocks, and simplify block arguments |
| Value simplification | Eliminate duplicate expressions, redundant concatenations, and algebraic identities |
| Commit optimization | Avoid unnecessary Stable/Working copies and sink commits toward their producers |
| Dead state removal | Remove writes that are not observable under the selected preservation policy |
| Scheduling | Reorder independent work while preserving data and memory dependencies |
| Layout requirements | Record proven state aliases for validation during physical layout |

Passes exchange analysis results through SIR- and design-owned identities. They
must not inspect Veryl syntax, assume x86 instruction costs as correctness facts,
or mutate a finalized physical layout.

Some transformations are ordered deliberately. For example, control-flow and
forwarding passes can expose dead values, while coalescing may create wider
operations that a later target either keeps or splits. The pass manager owns this
ordering; individual passes should not invoke one another as hidden pipelines.

## Physical layout boundary

Optimization may prove that two semantic state homes can share storage, but it
does not assign byte offsets. It emits a layout requirement, and
`celox-state-layout` validates width, state representation, and region
compatibility before applying the alias.

This separation keeps semantic rewrites independent of packing decisions and
prevents a backend from changing addresses after code generation begins.

## Native machine layer

The x86-64 backend lowers SIR into a private word-level SSA machine IR. At this
point it can use target facts that would be inappropriate in SIR, including:

- immediate instruction forms and x86 operand constraints;
- constant folding and algebraic simplification at machine width;
- copy propagation, global value numbering, and dead-code elimination;
- branch simplification and if-conversion;
- known-bits reasoning and redundant-mask elimination;
- target instruction selection for operations such as population count or bit
  extraction;
- pressure-aware scheduling, spilling, and register allocation.

Cranelift performs the corresponding target optimization through its own IR and
pass pipeline. WebAssembly generation likewise remains backend-private. No
machine-level result is written back into SIR.

## Runtime layer

Runtime optimizations avoid invoking compiled work when event semantics prove it
unnecessary:

- an event that does not match the required edge does not evaluate its domain;
- a non-cascaded single domain may use a combined evaluate-and-commit kernel;
- cascaded evaluation continues only while newly triggered domains exist.

These choices preserve the ordering model defined in
[Runtime Semantics](./cascade-limitations.md). They do not change signal values or
relax simulation consistency.

## Correctness boundary

Every compiler transformation must preserve observable state, events, errors,
and four-state value/mask behavior. A performance result can justify enabling or
disabling a legal transformation, but it is never evidence that an otherwise
unproven rewrite is correct.
