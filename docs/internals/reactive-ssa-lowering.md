# Reactive SSA lowering

> **Status:** architecture proposal and migration contract. The current native
> backend still composes the combinational and FF SIR CFGs sequentially. The
> first implementation step is limited to making that phase cut explicit and
> independently verified; it does not claim a runtime improvement.

## Motivation

The native clock-event fast path currently constructs:

```text
all combinational execution units
  -> publish combinational values through Stable Stores
  -> load those values in eval_apply_ff
  -> compute and stage next FF values
  -> publish FF state
```

The optimizer then tries to recover value flow from the Store/Load pairs,
delete publications which are not observed, sink selected producer cones, and
repair the resulting live ranges during allocation.

This order is backwards for code generation. Memory publication is part of a
simulator interface, but it is not the natural representation of an HDL value
inside one clock event. Once publication has become ordinary SIR Store/Load
instructions, later passes must rediscover:

- which Store and Load name the same logical range and phase version;
- whether an external observer can see the publication;
- which control path produced the value;
- whether carrying the value or rebuilding it at a sink is cheaper; and
- where the value should become physical memory rather than SSA.

The July 25, 2026 fixed release/LTO gate makes the remaining scale concrete:

```text
                         compile       execute
Veryl synchronous AOT-C  58.020 s      53.820 s
Celox native             54.407 s      59.616 s
```

Both runners completed at exactly `cy=9ae070 x3=aa pass=1`. Celox therefore
has a generated-execution deficit of 5.796 seconds, or 10.77%, while compiling
faster. Freeing the state-base register reduced pressure but did not remove
this residual architecture cost.

The failed profitability gate in
[Fused state SSA and code placement](./fused-state-ssa-placement.md) also
shows that bypassing only FF-suffix packed reloads is too narrow. This document
does not authorize that old Milestone 2. It replaces the unit of construction.

## Performance properties of the desired binary

The target is not merely fewer SIR instructions. A fast generated event
function should have all of these properties:

1. Values which remain internal to one event are SSA values, not public
   simulation-state round trips.
2. Only semantic event effects, externally observable publications, and
   phase barriers become Stores.
3. A value is computed under the control condition which needs it. Case arms
   which are not selected do not execute their payloads.
4. Independent logical values use native scalar widths while in registers.
   Packed layout is paid for only at an admitted physical boundary.
5. Producer and sink are close enough that register allocation sees short
   ranges. Shared source syntax does not force one machine live range.
6. Loop-carried values and simultaneous FF sampling remain explicit; neither
   code placement nor rematerialization may cross a phase version.
7. Code size stays bounded. Duplicating a small sink-local suffix is allowed;
   cloning a large shared decoder or whole combinational graph is not.
8. The scheduler receives a dependency-ready local packet. It is not asked to
   recover global HDL placement from one giant, already linearized function.

These are properties of the machine program. Store counts, VReg counts, and
SIR size are diagnostics only.

## The incorrect boundary

The parser is not required to emit machine order, and the machine backend must
not parse HDL syntax again. The missing layer is between those two extremes.

```text
Veryl/analyzer
  -> typed process CFG and effects
  -> Reactive SSA
  -> root-specific event projection
  -> machine CFG and sink packets
  -> MIR, allocation, x86
```

A parser expression DAG is insufficient because procedural priority, partial
writes, dynamic indices, loops, hierarchy, runtime observations, and
simultaneous FF sampling are control and state-version properties. Conversely,
ordinary SIR with every interface Store already materialized is too late.

Reactive SSA is therefore a typed process CFG with explicit state versions and
effects. It can initially be derived from verified SIR; the parser does not
need to be rewritten before the model is validated.

## Reactive SSA

### Value nodes

A value node has:

```text
ValueId
RegisterType
definition control region
operands
purity/effect class
source identity for diagnostics
```

Values wider than a machine word may have a legal chunk representation, but
logical widths are not VReg widths. A 27-bit HDL value remains a typed
27-bit value here and is lowered to one 32- or 64-bit machine value plus the
required normalization contract.

### State objects and versions

A state access names:

```text
StateObject
exact bit range or typed element range
phase
StateVersion
value/mask plane
```

The initial phases are:

- `PersistentInput`: state visible at event entry;
- `CombDerived`: a value produced while evaluating combinational logic;
- `NextFF`: a staged next-state value not yet visible to ordinary FF reads;
- `PublishedFF`: the version made visible by the event commit barrier.

A `StatePhi` represents path or loop merging. It is not an ambiguous set of
reaching definitions. Partial writes construct a new range version from the
previous version and inserted bits.

### Effects

Effects are explicit nodes ordered by an effect token:

- runtime error/assert/display/capture;
- trigger publication;
- dynamic or unresolved memory access;
- externally required combinational publication;
- `StageNextFF`;
- `CommitFFState`.

`StageNextFF` has an earliest point after its value, guard, and state
dependencies and a latest point before `CommitFFState`. The commit is a fixed
phase barrier which publishes the final version of every staged FF range.

### Materialization sources

A state or value edge may end at an executable materialization source:

```text
Constant
DominatingSSA
ReadPersistentState
ReloadPreservedHome
ControlMerge
```

Naming a StateVersion is not enough. Every source states how the value is
obtained at its use cluster and proves dominance, phase equality, range
identity, and control legality.

## Root-specific projections

One elaborated Reactive SSA graph produces multiple executable projections.

### Combinational projection

Roots are:

- public combinational outputs required by the simulator interface;
- triggers, captures, observations, and runtime effects;
- convergence/SCC effects.

This projection preserves the current generic `eval_comb` contract.

### Clock-event projection

Roots are:

- FF next-state values for the selected clock/reset domain;
- `StageNextFF` and `CommitFFState`;
- event-local observations and runtime effects;
- comb publications which are independently proved observable before the
  simulator marks combinational state dirty.

An ordinary comb Store is not inherited from the combinational projection.
If a comb-derived value feeds only an FF next-state expression, the clock
projection contains a value edge, not a Store followed by a Load.

### Split evaluate/apply projection

Cascade and simultaneous-domain execution still require:

- an evaluate projection which stages `NextFF` without publication; and
- an apply projection containing only the publication barrier and required
  trigger effects.

The single-domain fused projection is an optimization of the same phase graph,
not a separately parsed semantic program.

## Projection construction

For one requested projection:

1. Select semantic effect roots and requested state publications.
2. Walk backward through exact SSA and StateSSA def-use edges.
3. Retain required control dependence and loop-carried SCC edges.
4. Stop at executable materialization sources.
5. Partition uses into sink clusters by control region, loop context, and
   effect interval.
6. Choose for each cluster:
   - carry a dominating value;
   - rematerialize a bounded pure suffix;
   - reload an exact preserved home; or
   - retain the original packed path.
7. Construct machine CFG only for the retained projection.
8. Emit bounded sink packets to machine scheduling and allocation.

This is sparse graph reachability. It does not enumerate paths and does not
construct a whole-function scheduling DAG.

## Placement and allocation

Global placement and machine scheduling remain distinct.

Reactive SSA chooses a legal control region and materialization source. The
machine scheduler orders only the instructions in a bounded packet whose
external values and effects are already explicit. Register allocation may
downgrade:

```text
DirectForward -> Rematerialize -> ReloadPreservedHome
DirectForward -> ReloadPreservedHome
```

It may split a use cluster but may never merge clusters or silently recreate
one long shared live range. Plan-local rematerializations have stable
identities so ordinary CSE cannot merge them across cluster boundaries.

The allocator remains responsible for:

- exact point/edge homes;
- fixed-register constraints;
- spill decisions;
- phi-edge parallel copies; and
- final physical assignment.

It is not responsible for deciding which HDL case arm or combinational
publication belongs in the event.

## Complexity and memory bounds

Let `V` be retained value/effect nodes, `E` retained SSA/control edges, `R`
range endpoints, and `P` emitted machine instructions.

- state-range indexing: `O(R log R)` construction;
- sparse StateSSA: `O(V + E)` over accessed ranges;
- root projection: `O(V + E)` in the visited subgraph;
- SCC discovery: `O(V + E)`;
- packet construction: `O(P + dependency edges)`;
- resident analysis storage: `O(V + E + R)`.

The implementation must not allocate:

- a value-by-block matrix;
- all-pairs alias or dominance tables;
- one cloned graph per sink;
- complete path sets; or
- a second full instruction order.

Temporary projection tables are dropped before MIR allocation. Stage RSS is
reported separately from peak RSS.

## Migration

### Step A: explicit fused phase cut

Replace the positional `Option<usize>` suffix convention with a typed clock
event composition. Verify:

- the FF entry exists;
- every FF-reachable block remains inside the FF suffix;
- no FF edge returns to a comb block;
- the phase classification covers every merged block; and
- generic chained functions do not acquire an implicit phase.

This step changes no generated instructions. Its purpose is to prevent future
projection code from inferring phase semantics from unit order.

### Step B: projection oracle

Build Reactive SSA from the current merged SIR without rewriting it. Emit, for
each projection:

- roots by effect class;
- retained values, blocks, and ranges;
- comb publications absent from the clock projection;
- materialization frontier sources;
- loop/control cutoffs; and
- bounded memory/time measurements.

The existing generated SIR/MIR remains the executable reference.

### Step C: straight-line static clock projection

Generate code directly for a deliberately small subset:

- two-state;
- static non-overlapping ranges;
- acyclic control region;
- no capture/trigger/runtime effect between producer and FF sink;
- exact persistent input or dominating SSA frontier;
- bounded sink-local recipe.

Compile both the old fused function and the new projection in tests. Execute
randomized input states and compare final Stable/Working/triggered memory plus
status.

### Step D: control-pure regions

Add exact branch priority, `ControlMerge`, and loop-independent case regions.
Do not speculate payloads. Each selected arm executes only its own recipe and
rejoins one verified continuation.

### Step E: replace the fused fast path

Use the clock projection for admitted domains while retaining the old fused
function as a differential oracle in tests. Generic `eval_comb`,
`eval_only_ff`, and `apply_ff` remain unchanged until their corresponding
projections pass independently.

Only after the complete semantic gate may the old fused concatenation path be
removed.

## Gates

Every code-generating step runs:

- focused projection and phase tests;
- full `celox` library tests;
- native execution and native testbench integration tests;
- exact Heliodor `cy=9ae070 x3=aa pass=1`;
- separated compile and execute timing; and
- complete final SIR/MIR inspection.

The final release/LTO gate compares generated execution, not process or
compile time. A transformation is not accepted merely because it removes
Stores: it must improve the identified machine work without increasing
spills, code size, or unselected control work enough to lose the gain.

## Rejected shortcuts

- **Inlining comb syntax into FF parsing:** duplicates HDL semantic analysis
  and loses verified CFG/effect structure.
- **Deleting all comb Stores in the fused function:** external observations,
  triggers, and dynamic aliases remain real roots.
- **Forwarding every Store to every FF Load:** recreates whole-function live
  ranges and allocator spills.
- **One giant expression DAG:** cannot represent effect order, partial writes,
  loops, or simultaneous FF sampling and has unacceptable scheduling scale.
- **Arbitrary allocation regions:** partitioning after placement cannot recover
  the missing event projection.
- **Function splitting as the primary fix:** it can change I-cache and
  allocation behavior but does not remove semantic work which should never
  have entered the clock projection.

## Related documents

- [Fused state SSA and code placement](./fused-state-ssa-placement.md)
- [Native throughput execution plan](./native-throughput-execution-plan.md)
- [Native register allocation](./native-register-allocation.md)
- [Simulator architecture](./architecture.md)
