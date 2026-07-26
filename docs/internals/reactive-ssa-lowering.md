# Reactive SSA lowering

> **Status:** superseded as the construction architecture by
> [Event IR (EIR)](./event-ir.md). This document records the earlier
> SIR-derived investigation and its measurements. In particular, its proposal
> to derive the semantic graph from merged SIR is not the target pipeline.
>
> **Historical status:** architecture proposal and migration contract with the first
> production sink-local rewrite enabled. The current native backend still
> composes the combinational and FF SIR CFGs sequentially; the admitted
> two-state static subset removes individually proved Store/Load round trips
> without claiming that the full projection architecture is complete.

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

An `ExecutionUnit` is only a physical code-generation container. In
particular, putting all acyclic combinational logic in one EU must not collapse
the semantic units inside it. The lowering pipeline preserves this hierarchy:

```text
comb process
  -> exact LogicPath/range definitions
  -> scheduler SCC and effect interval
  -> SIR definition sites
  -> MemorySSA StateVersions
  -> FF demand cluster
```

The clock projection and placement planner operate on the lower levels of this
hierarchy, never on the whole comb EU as one scheduling or allocation region.
Several independent FF demand clusters may refer to definitions contained in
the same comb EU without acquiring a shared live range or a common
materialization decision.

Semantic-region identity is retained for every elaborated comb process and
for otherwise ungrouped continuous/glue LogicPaths. It is a clustering and
placement hint, not a correctness proof. Optimizer-created aggregate or
repartitioned Stores may not have one unique source region; their legality is
still decided solely by exact MemorySSA versions and effects.

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

The first implementation represents every FF state Load as an independent
MemorySSA demand root. Its reaching `Def`, `MemoryPhi`, or `LiveOnEntry`
version is expanded without treating a merge as ambiguous. A silent comb
publication `Def` crosses into the defining SSA value; an observable
publication remains an effect frontier. This establishes clusters before any
Store deletion or code motion is attempted.

On the Heliodor Linux workload, the first demand-cluster trace reports:

```text
FF state demand clusters       3,704
complete clusters              3,242
comb-publication-backed        2,017
pure suffix instructions      41,821
largest pure suffix              579
SIR control merges             1,880
unsupported frontiers            462
```

After connecting source semantic regions to MemorySSA definitions, 1,824 of
the 2,017 publication-backed clusters have an unambiguous region identity.
The remaining 193 include definitions whose optimized range no longer has one
unique containing source range. They remain valid MemorySSA clusters and must
not be rejected or guessed solely from provenance.

Point-specific MemorySSA checks of producer-cone Load frontiers report 17,030
state Load leaves. Of these, 15,582 (91.5%) observe exactly the same
StateVersion at the target FF use cluster and may be reloaded there. The
remaining 1,448 must retain an earlier value or stop at a version-valid
frontier; moving them unconditionally would change RTL semantics. Queries use
a sorted per-block/per-slot definition index rather than rescanning all
MemorySSA accesses per leaf.

The whole retained projection still contains 13,040 instructions and exceeds
the obsolete 4,096-instruction whole-projection limit. No individual demand
cluster does. This is evidence for sink-local construction, not yet a runtime
speedup: shared producer accounting, cluster partitioning, and executable
control placement are still required before code generation.

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

#### Implemented analysis contract

The first projection oracle is now built from the exact merged SIR before any
merged-chain rewrite. It is captured only by an explicit native IR trace and
does not add analysis time to ordinary production compilation.

The graph combines:

- exact source-EU and comb/FF block provenance;
- SIR SSA value and block-parameter edges;
- sparse StateSSA versions for stable, working, and sparse-working state;
- exact cross-region Commit copies;
- FF publication Commit, observable Store, runtime event, capture, and error
  roots; and
- live-on-entry, unresolved alias, loop-control, and unsupported-access
  frontiers.

`Commit(stable -> working)` is a state-copy edge, not a publication root.
`Commit(working|sparse-working -> stable)` is a publication root. This
distinction ensures that the projection starts from final published FF
versions and reaches only the staged definitions which can affect them,
instead of treating every intermediate working Store as a sink.

The implementation uses one work-list visit per retained SSA instruction and
control block. It does not revisit a shared producer once per root and does
not construct full control-dependence or value-by-block matrices.

The native trace writes the complete report to
`reactive_event_graph.txt`. For the Heliodor Linux workload at
`7ad830fc0f8506c934b61a853ce2eadfa5926b82`, the clock projection completed
inside the existing full trace compile and reported:

```text
units                       37  (1 comb, 36 FF)
semantic roots             719
retained blocks           1,131
retained instructions    13,040
materialization cutoffs   2,153
cross-EU state flows         45
```

The 45 exact cross-EU flows connect the comb EU to FF EUs by state range and
StateVersion. The cutoff set contains 1,765 live-on-entry sources, 232 real
memory kills, and 156 loop-control boundaries. These are feasibility facts,
not a speedup claim: Step C must still turn each admitted frontier into an
executable materialization source and compare the resulting state/effects
against the existing fused function.

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

The first production rewrite admits only a bounded, branchless SIR projection
with exact static StateSSA edges and executable sink-local frontiers. For an
exact comb-Store to FF-Load edge it:

1. is disabled for four-state execution until value and mask planes can be
   proved and deleted atomically;
2. requires identical static address, range, and SIR value type;
3. requires an effect-free comb Store and one exact FF Load consumer;
4. rejects any other static or dynamic read overlapping the Store range,
   including differently shaped accesses which MemorySSA represents as kills;
5. clones at most 16 pure SSA instructions, including definitions from
   dominating predecessor blocks, immediately before the FF Load;
6. requires every state Load frontier to observe the same event StateVersion
   and the same physically reloadable StateVersion at the sink;
7. computes which original cone instructions become dead after deleting the
   publication and admits the rewrite only when the cloned cone is no larger
   than the dead cone plus the removed Store and Load;
8. removes the FF Load and comb Store together; and
9. runs ordinary SIR DCE and verification.

A focused differential test executes the old fused SIR and projected SIR over
several input states, then executes the ordinary comb projection before
comparing state. This models the simulator's dirty-comb contract: a clock-only
projection need not publish an unobserved intermediate comb value, but the
value must be reconstructed before an external observation.

An additional regression preserves a 64-bit publication when a 12-bit
overlapping Load also consumes it. The initial implementation checked only the
64-bit MemorySSA Def's exact consumers and deleted the whole Store, delaying
the Heliodor Linux completion marker by one 10,000-cycle polling interval.
Inspection of the complete generated SIR exposed the differently shaped Load;
the range-wide read index above closes that hole.

The initial production subset incorrectly restricted every producer cone to
the Store's basic block and therefore admitted only 114 clusters. Removing
that restriction without a profitability contract admitted 285 clusters, but
duplicated shared, already optimized packed-bit cones. Although memory
operands decreased, the final x86 function grew by 342 instructions:
`and +105`, `test +73`, `shr +45`, and `xchg +47`. Store/Load elimination had
been traded for repeated extraction work and additional allocation copies.

The dead-cone contract above admits 160 clusters on the Heliodor Linux
workload. Relative to the phase-cut baseline, native optimized SIR decreases
from 18,811,600 to 18,784,955 bytes and MIR from 54,736,563 to 54,695,093
bytes. Relative to the old 114-cluster subset, the final x86 function has 105
fewer instructions, 101 fewer memory operands, 84 fewer GS state accesses, and
47 fewer memory-source `movzx` instructions. A non-LTO optimized
qualification completed kernel power-down at the exact
`cy=9ae070 x3=aa pass=1` marker, with 75.710 seconds of code generation and
62.194 seconds of generated execution. The release/LTO qualification reached
the same exact marker with 67.231 seconds of code generation and 61.940
seconds of generated execution. Host timing varies by several seconds; the
structural machine-code deltas and exact cycle marker are the acceptance
evidence here, not either single wall-clock sample.

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
