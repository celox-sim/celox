# Event IR (EIR)

> **Status:** semantic and lowering contract. EIR is the required pre-SIR
> boundary for the clock-event path.

## Name

The new intermediate representation is named **Event IR**, abbreviated
**EIR**.

An EIR graph describes the values, process-local versions, effects, and phase
barriers required to execute one simulator event. It is not machine CFG, not
physical simulator memory, and not a reconstruction of already emitted SIR.

The name deliberately does not contain `SSA`. Ordinary EIR values and
process-local variables use SSA identity, but persistent RTL state is modeled
as an immutable event-entry snapshot plus explicit staged updates and a commit
barrier. Calling the whole representation `EventSSA` would obscure that phase
distinction.

## Position in the pipeline

AIR, SLT, EIR, and SIR have different responsibilities:

```text
Veryl source
    |
    v
AIR  (typed HDL declarations, expressions, procedural control, effects)
    | \
    |  \ combinational symbolic lowering
    |   v
    |  SLT + LogicPath
    |   (pure/symbolic value recipes and range definitions)
    |          \
    |           \
    +------------> EIR
                   (one elaborated event's semantic value/effect graph)
                         |
                         | projection, placement, and CFG construction
                         v
                       SIR
                   (executable CFG, registers, explicit memory operations)
                         |
                         v
                       MIR / native code
```

This is not an `AIR -> SLT -> SIR` pipeline. Only combinational AIR is
symbolically represented by SLT. FF procedural control and effects remain AIR
input to EIR. EIR is formed from **both** sources:

- SLT/LogicPath supplies combinational value definitions.
- AIR supplies FF process control, process-local versions, staged state
  updates, runtime effects, and source-language priority.

SIR is a lowering result of EIR. SIR is not an input from which EIR should
rediscover ordinary comb-to-FF value flow.

## Responsibility of each representation

| Representation | Owns | Must not decide |
|---|---|---|
| AIR | typed source constructs, procedural order, reset/branch/loop semantics, declared effects | simulator memory layout, machine placement |
| SLT | symbolic combinational expressions, exact logical widths, range-valued LogicPath definitions | FF staging visibility, commit order, physical publication |
| EIR | event-entry snapshot, resolved comb definitions, process-local SSA, effect order, staged FF updates, event projections | physical offsets, VRegs, final instruction order |
| SIR | executable CFG, virtual registers, explicit semantic Load/Store/Commit operations | which HDL value a Store/Load pair originally represented |
| MIR | machine-width operations, VRegs, constraints and allocation input | HDL process or event semantics |

## EIR scope and identity

The canonical EIR graph is built after elaboration and hierarchy flattening.
Logical objects therefore use `AbsoluteAddr`, exact bit or element ranges, and
an explicit event phase. Module-local AIR and SLT fragments retain source
identity until they are relocated into that graph.

An EIR graph belongs to one semantic event domain:

- generic combinational evaluation;
- one clock/reset domain.

`FusedClock`, `EvaluateClock`, and `ApplyClock` are executable projections of
one clock-domain graph. They are not separate EIR graphs. In particular, the
evaluate and apply projections refer to the same `StageNextFF` identities and
the same `CommitFFState` barrier.

## Semantic namespaces

EIR keeps four namespaces separate.

### `ClockSnapshot`

`ClockSnapshot` is the immutable state visible at event entry. It contains FF
state, simulator inputs, persistent memories, and other committed state.

An FF process cannot observe a `StageNextFF` written by another process in the
same event. It continues to read `ClockSnapshot` until the commit barrier.

### `CombDefinition`

A `CombDefinition` names a settled combinational range and its value recipe.
Its recipe is the intact flattened SLT root plus the `LogicPath` bindings
resolved by EIR construction. A comb definition is a logical value, not a
simulator-memory home.

The shared `CombGraph` records, for every recipe:

- current-value edges to exact `CombDefinition` ranges;
- uncovered event-entry snapshot ranges;
- explicit previous-value snapshot ranges;
- dynamic-address provenance;
- process-local SLT substitutions;
- source-order edges; and
- combinational convergence membership.

Recipe node IDs refer to the same immutable flattened SLT arena retained by
the compilation, so importing EIR does not clone the 100k-node recipe arena.

For acyclic logic, the definition may be expanded directly into its value
dependencies. A combinational SCC is represented by a convergence region and
its result boundary; EIR must not inline through the iteration boundary as if
it were an acyclic expression.

### `ProcessLocal`

`ProcessLocal` contains versions created while interpreting one AIR process.
It preserves the exact procedural semantics of assignments, branches, loops,
functions, reset priority, and local temporaries.

Process-local versions never become visible to another FF process merely
because the two processes share a clock. Any communication between processes
uses `ClockSnapshot`, a settled `CombDefinition`, or an explicitly defined
language-level effect.

### `StagedState`

`StageNextFF` records a guarded, range-qualified candidate for the next FF
state. It carries source-process identity and write priority where overlapping
or partial updates require it.

Staged state is not an ordinary value namespace. It becomes visible only
through `CommitFFState`.

## Core node classes

### Values

Initial EIR value nodes are:

```text
Constant
ReadClockSnapshot(object, range)
ReadPersistentMemory(object, access)
ReadCombDefinition(definition, range)
Unary
Binary
Compare
Mux
Slice
Concat
DynamicSelect
ProcessPhi
LoopValue
```

An SLT `Input` is not lowered immediately to a memory Load. Its range is bound
to one or more exact `CombDefinition`s, or to the uncovered part of the
event-entry snapshot. A `previous_sources` input always names the snapshot
version even when a combinational definition exists.

AIR expression lowering produces the same EIR value operations while using
the current `ProcessLocal` environment.

### Effects

Effects are explicit and ordered:

```text
StageNextFF
WritePersistentMemory
RuntimeEvent
Capture
TriggerPublication
```

Effects carry guards and effect-token dependencies. Pure value edges do not
implicitly establish effect order.

### Barriers and regions

```text
CombConvergenceRegion
CommitFFState
RuntimeObservationBarrier
```

`CommitFFState` is a fixed phase barrier. Every FF RHS and every
`StageNextFF` required by the projection precedes it. Ordinary snapshot reads
cannot be moved after it while retaining their old version.

## AIR import rules

AIR is interpreted into EIR without sharing one mutable environment between
comb and FF logic.

For one FF process:

1. Create a fresh `ProcessLocal` environment.
2. Resolve a read of persistent/FF state to `ClockSnapshot`.
3. Resolve a read of a combinationally driven signal to the corresponding
   settled `CombDefinition`.
4. Resolve a read of a process-local variable to the current local SSA
   version.
5. Lower procedural branches and loops while preserving AIR priority and
   control dependence.
6. Convert FF destination updates to `StageNextFF`; do not mutate
   `ClockSnapshot`.

Each FF process gets a separate local environment. Joining all FF AIR into one
mutable SymbolicStore is invalid because it can expose one process's staged
write to another process during the same event.

Partial writes construct an explicit update recipe:

```text
UpdateRange(base_snapshot_or_local_version, range, value, guard, priority)
```

Dynamic writes retain their address expression and conservative alias range.
They are not expanded into one candidate per possible bit.

## SLT import rules

SLT remains the source representation for combinational value recipes.

For every required LogicPath range:

1. Retain its SLT root and exact target range as one recipe and
   `CombDefinition`.
2. Resolve every current source range through the disjoint range-definition
   index; retain uncovered subranges as event-entry snapshot inputs.
3. Retain every previous-value range as a distinct snapshot input.
4. Preserve semantic process identity, local-input substitutions,
   dynamic-address provenance, auxiliary/effect roots, and source-order edges.
5. Compute convergence regions from value-definition edges only.
   Anti-dependence/source-order edges constrain scheduling but do not turn a
   procedural ordering cycle into a combinational fixed-point SCC.

Multiple EIR use clusters may later materialize the same logical EIR value
independently. Logical value identity does not force one VReg or one placement.

## EIR projections

### Generic combinational projection

Roots are persistent signal values required at the event boundary,
convergence effects, captures, triggers, and runtime effects. Persisting a
boundary value is a projection decision; it is not modeled as a source-level
comb publication effect.

### Fused clock-event projection

Roots are the selected domain's staged next-state values, required event
effects, and `CommitFFState`. A comb value used only by an FF RHS remains a
value edge:

```text
CombDefinition -> FF expression -> StageNextFF
```

It does not become:

```text
comb Store -> FF Load
```

### Split evaluate/apply projections

The evaluate projection computes and stores staged values without publishing
them. The apply projection performs the commit and required publication
effects. This preserves simultaneous-domain and cascade semantics.

## EIR to SIR lowering

Lowering performs root selection, use clustering, placement, and CFG
construction before emitting SIR.

It may choose per use cluster to:

- retain a dominating value;
- rematerialize a bounded pure recipe;
- read immutable snapshot/persistent state; or
- create an explicit compiler-selected home.

SIR memory operations are emitted only for:

- snapshot or persistent-memory reads;
- staged/persistent writes;
- externally required publications;
- semantic effects and phase commits; or
- an explicit materialization decision.

Spills caused by physical register pressure are a MIR/register-allocation
decision and must not be pre-created as public RTL state traffic.

EIR lowering preserves structured control until deciding whether a Mux should
remain dataflow or become CFG. It does not first create one giant branchless
SIR graph and ask register allocation to repair its live ranges.

## Correctness invariants

An EIR implementation must verify:

1. Every value use has one width- and type-correct logical definition.
2. Every FF/persistent read names the correct event-entry phase.
3. No `StageNextFF` is visible through `ClockSnapshot`.
4. Process-local versions do not escape their process without an explicit
   effect or staged update.
5. Overlapping staged writes preserve AIR guard and priority semantics.
6. Every required staged range reaches exactly one final commit recipe.
7. No value/effect crosses a combinational convergence barrier illegally.
8. Runtime observations retain their AIR order and control dependence.
9. Four-state value and mask information is transformed atomically.
10. Projection changes may remove only effects absent from that projection's
    observable contract.

## Role of MemorySSA

MemorySSA is not used to reconstruct ordinary comb-to-FF value flow. That flow
is explicit in EIR before SIR exists.

MemorySSA may still be used after lowering for:

- true persistent memory and dynamic alias analysis;
- verification of explicit SIR memory effects;
- optimization of compiler-selected materialization homes; and
- checking transformations after CFG mutation.

It is a memory/effect tool, not the semantic bridge between SLT and FF AIR.

## Cutover boundary

Implementation and cutover proceed as follows:

1. Retain AIR process identity until EIR construction.
2. Import flattened SLT/LogicPath definitions.
3. Make EIR construction total over every AIR/SLT construct accepted by Celox.
4. Build and verify complete EIR graphs.
5. Compare complete observable state/effects in whole-pipeline equivalence
   tests.
6. Switch SIR construction atomically to EIR after semantic coverage is
   complete.
7. Delete the direct AIR-to-SIR and SLT-to-SIR construction paths.

Failure to construct or verify EIR is a compile error. The implementation
must not mix EIR and legacy writes or select a lowering path by event group.
