# Fused state SSA and code placement

> **Status:** Milestone 0 feasibility analysis is approved. Later milestones
> require the measured coverage result and a reviewed materialization/staging
> specification before implementation. This document does not authorize a
> production switch. Each code-generating milestone has an independent
> semantic, generated-code, and Linux execution gate.

## Objective

The native fused clock-event function currently carries many combinational
values through packed simulator memory:

```text
old FF/input state
  -> combinational operations
  -> STABLE store
  -> STABLE load
  -> extract/merge operations
  -> next-FF operations
  -> StageNextFF
  -> CommitFFState
```

For a combinational value used only to compute next FF state, the desired
shape is:

```text
old FF/input state
  -> combinational operations
  -> next-FF operations
  -> StageNextFF
  -> CommitFFState
```

Combinational values are derived values, not persistent simulator state.
Materialization is required only by a real observation or effect, or when the
machine backend deliberately chooses a home for a value which is more
expensive to retain or recompute.

This design specializes the already fused
`eval_comb_apply_ff` SIR. It does not change parsing or the generic
`eval_comb`, split `eval_only_ff`, or `apply_ff` paths in its first
implementation.

The intended phase order is:

```text
range-aware StateSSA and sink-rooted ADCE
  -> use-cluster formation
  -> materialization planning
  -> plan-constrained global placement
  -> block-local scheduling and pressure validation
  -> monotonic plan repair when required
  -> physical register allocation
```

## Measured scale

The complete Step 77b Heliodor trace at commit `e34bd4d8` contains the
following `eval_comb_apply_ff[0]` input:

| Stage | Instructions | Blocks |
|---|---:|---:|
| Native-optimized SIR | 96,152 | 13,987 |
| MIR immediately after ISel | 97,768 | 12,333 |
| MIR after CFG normalization and pressure scheduling | 104,711 | 22,960 |

The counts establish two constraints:

1. whole-function sparse CFG and SSA analyses are practical;
2. a whole-function instruction scheduling DAG, pairwise interference graph,
   or speculative graph partition search is not.

The complete profile also showed that Celox executes approximately 4.4 times
as many `and` instructions, 5.7 times as many shifts, 16 times as many
zero-extending loads, and 19 times as many conditional moves as the Veryl C
path. These ratios motivate the investigation but do not by themselves prove
that packed state traffic causes every difference. Milestone 0 must attribute
candidate instructions to packed address calculation, range extraction,
range insertion, mask generation, memory traffic, mux lowering, or unrelated
arithmetic before this design claims explanatory coverage.

## Terminology

This design introduces no `EventSSA` IR.

- **Fused SIR** is the existing SIR formed from the combinational EUs followed
  by one `eval_apply_ff` event.
- **StateSSA** is the version graph for simulator state ranges.
- **StateVersion** is a logical range value and its defining recipe. It is not
  a storage location.
- **MaterializationSite** is an actual register, preserved state Store,
  compiler-private scratch slot, or rematerialized expression which can supply
  a StateVersion to a use.
- **Promoted range** is a StateSSA version represented by ordinary SIR SSA
  values at selected uses.
- **Materialization** is an explicit load, store, or rematerialized expression
  selected for a particular use cluster.
- **Sink** is an operation whose result or effect is semantically required.

## Non-goals

The first implementation does not:

- construct a new expression DAG in the parser;
- replace the generic `eval_comb` execution path;
- demand-evaluate public signals through a new runtime API;
- schedule all fused instructions in one global ready queue;
- partition an arbitrary dependency graph into invented allocation regions;
- promote dynamic, effectful, or four-state accesses without a proof;
- remove a state store before a materialization planner supplies every retained
  use cluster;
- depend on an arbitrary function-size, block-count, traversal, or iteration
  cap for correctness or termination.

Profitability decisions may conservatively leave a range memory-backed. For
example, a dynamic access with 100,000 candidate indices must not be expanded
merely because the algorithm can eventually finish. The analysis itself must
terminate without a cap; a cost model may reject expensive promotion.

## Current behavior and missing capability

The parser deliberately emits three FF forms. `eval_apply_ff` contains the
STABLE-to-WORKING seed, next-state evaluation, and WORKING-to-STABLE commit.
The native backend merges combinational EUs and one `eval_apply_ff` group into
one SIR function.

After merging, production performs exact-fragment WORKING StateSSA promotion.
It can remove an eligible:

```text
STABLE -> WORKING seed
WORKING load/store
WORKING -> STABLE apply
```

round trip. The older direct rewrite handles remaining exact cases.

Cross-phase STABLE forwarding is disabled. Its earlier trial replaced loads
but exposed definitions as long whole-function live ranges, increasing spill
pressure without improving execution. The disabled switch is not a missing
one-line optimization; it identifies the required contract between StateSSA,
code placement, and allocation.

The program-level rooted DSE is address based. A combinational store remains
live when any FF EU loads its address. It therefore preserves the exact
Store/Load pair which the fused function should bypass. Subsequent ordinary
DCE cannot remove the producer because the retained store remains an effect.

## Semantic roots

Specialization starts from an explicit root set in the fused clone.

Required roots are:

- movable FF next-state staging writes;
- the fixed FF commit/publication barrier at the kernel suffix;
- RAM and other persistent-state writes;
- trigger and capture effects;
- runtime events and error exits;
- sparse active/dirty publication effects;
- any output explicitly required by the fused-call contract.

An ordinary combinational Store is not a root merely because the generic
`eval_comb` stores the same address. The generic function remains unchanged.
The fused clone independently proves whether that store is observable between
its combinational prefix and FF suffix.

`StageNextFF(q, value)` consumes a completed RHS value and may move within the
evaluation phase. `CommitFFState` publishes every staged value and must remain
after all FF RHS evaluation. The root builder and effect model must distinguish
the two; a generic Store opcode is insufficient.

The root builder must also verify that no runtime callback, capture, trigger,
cascade boundary, or phase bypass observes the candidate state range.

## Range-aware StateSSA

### Logical identity

A state version is identified by:

```text
storage object
region and simulation phase
value or mask plane
bit interval
```

Machine chunks are not logical identities. A 27-bit range is not given a
27-bit machine register class, and arbitrary-width values are not eagerly
assembled into one host value.

### Access-boundary partition

For one static object, collect every static access endpoint and form disjoint
logical ranges only at those endpoints. For `A` accesses, the number of
distinct segments is `O(A)`, independent of the numerical width of the object.
That bound does **not** make an eagerly expanded version graph sparse. A
sequence alternating narrow endpoint-producing writes with whole-object
reads/writes can make access-to-segment overlap edges `O(A^2)`. Placing one
range phi at many joins can likewise create
`O(segments * joins)` phi operands.

```text
Store x[63:0]
Load  x[15:8]
Store x[23:16]
```

becomes range definitions and uses over the access-boundary partition. The
narrow load is no longer a mismatched-width kill of the full store.

Endpoint sorting costs `O(A log A)` per object. Overlap lookup uses the shared
unit-independent interval index in `celox-analysis`; it must never allocate a
bitset proportional to declared RTL width.

Define the materialized range-graph size as:

```text
R =
    generated range fragments
  + access/fragment overlap edges
  + range-phi operands
```

All complexity and memory claims after endpoint collection are expressed in
terms of `R`, not just SIR instruction count `N` or access count `A`.

Milestone 0 must compare two non-eager representations before selecting the
production form:

1. persistent interval maps in which a write records only changed interval
   nodes and shares unchanged ranges;
2. demand-driven reaching-definition queries which instantiate only ranges
   requested by semantic sinks.

Neither representation may first expand every access over every segment and
then attempt to compress the result.

### Version graph

The analysis creates sparse:

- `MemoryDef` nodes for range writes;
- `MemoryUse` nodes for reads and sink demands;
- `MemoryPhi` nodes at pruned dominance-frontier joins;
- `Kill` nodes for accesses whose overlap cannot yet be proved.

Loops remain CFG/MemorySSA SCCs. The implementation does not attempt to turn
the complete function into an acyclic expression graph.

### Initial admissible subset

The first rewriting subset is:

- static addresses and static bit ranges;
- two-state storage;
- no trigger, capture, or runtime effect on the access;
- no unknown pointer alias;
- phase ordering proved by the fused boundary;
- every removed store covered by an independently verified sink/writeback
  proof.

Dynamic or four-state access does not invalidate unrelated objects or
disjoint static ranges. It inserts a conservative kill for its proved alias
set. Improving that alias set is a later coverage task, not a correctness
shortcut.

## Sink-rooted liveness and ADCE

Starting from the semantic roots, traverse:

- register def-use edges;
- StateSSA version edges;
- phi incoming edges;
- required control-dependence edges;
- ordered effect dependencies.

The traversal is `O(N + R + E)` in the materialized graph. It produces a
rewrite plan; it does not choose an instruction order.

The plan removes an ordinary comb Store only if every required consumer reads
the same promoted range version and no observation requires the memory value.
After applying the complete plan atomically, ordinary DCE removes unreachable
producer computations and dead control regions.

The original generic `eval_comb` remains the semantic reference and retains
its memory-backed outputs.

## Use clustering and materialization planning

StateSSA versions are logical values, not storage locations. A StateVersion
alone is never a valid reload source. Before deleting a Store which provides
the only memory materialization of a version, the planner must select a source
and exit action for every retained use cluster.

Use clusters are formed from exact ordinary and phi-edge uses using CFG
dominance, loop membership, and block proximity. Clustering does not create
one mandatory VReg spanning the clusters.

The explicit plan has this shape:

```text
UseClusterPlan {
    version_id
    cluster_id

    source:
        CarryFromProducer
        Rematerialize(recipe)
        ReloadPreservedState(site, version)
        ReloadPrivateScratch(slot)

    exit:
        Dead
        CarryToCluster(next)
        StorePreservedState(site)
        StorePrivateScratch(slot)
}
```

`ReloadPreservedState` is legal only when an explicit original or relocated
Store remains and MemorySSA proves that the requested version reaches the
load. Deleting that Store deletes the home. `ReloadPrivateScratch` requires an
explicit compiler-added Store whose reaching definition is independently
verified. `Rematerialize` names a pure executable recipe, not merely a logical
StateVersion.

The materialization planner runs before global placement. It chooses an
initial plan from:

- expression and memory-operation cost;
- use-cluster frequency and loop depth;
- estimated register-class pressure at cluster boundaries;
- code-size cost of cloning a producer;
- availability of preserved state and private scratch homes.

Placement and local scheduling then validate the plan against actual
register-class budgets. An infeasible plan is repaired with a restricted,
monotonic operation:

- change a carry edge to rematerialization;
- change a carry edge to an explicit scratch or preserved-state home;
- split a use-cluster edge;
- preserve or introduce an explicit Store;
- clone a pure shared producer.

Within one planning attempt, repair never changes a materialized edge back to
the failed carry edge. The finite set of cluster edges and source choices
therefore proves termination without an iteration cap. A later profitability
pass may start a new plan, but correctness never depends on finding an optimal
fixed point.

## Global code placement

ADCE answers which operations remain, and the materialization plan decides
which value must exist in each use cluster. Placement then solves the observed
problem where a combinational definition is far from its Store or FF use.

Pure promoted operations use early/late global code motion over the existing
CFG.

### Earliest point

The earliest legal block is dominated by every operand definition and is
after every required state version. Loads may move only where their exact
MemorySSA version remains valid.

### Latest point

The latest legal block dominates every use selected by the cluster plan. It is
based on the nearest common dominator of ordinary and phi-edge uses. A value
with one ordinary comb backing Store or movable `StageNextFF` use may therefore
use that Store's block as its latest legal block. `CommitFFState` is not such a
Store: it remains a fixed suffix phase barrier and is never moved to the
producer.

### Placement choice

Choose a point between earliest and latest using:

- loop depth and measured or static block frequency;
- execution count changes caused by branch sinking;
- live-range pressure;
- expression/rematerialization cost;
- code-size cost when duplication is considered.

A pure single-use cone is recursively placed next to its sink. A cheap shared
cone may be cloned into separate use clusters. An expensive shared cone stays
at a common dominator only when the materialization plan carries it across
clusters; otherwise each later cluster receives an explicit rematerialization
or reload source. GCM alone is never used to justify a long cross-cluster live
range.

Code motion must not speculate:

- trapping or target-constrained machine operations;
- runtime or capture operations;
- stores or unknown loads;
- an arm-specific computation into an unconditionally executed predecessor.

This is an ordinary SSA placement problem. It is not delegated to register
allocation and is not approximated by a Store-priority rule in the current
ready queue.

## Block-local machine scheduling

Global placement preserves the CFG. Machine scheduling constructs a sparse
dependency DAG per basic block, not per function.

MemorySSA/effect dependencies order aliasing loads and stores. Disjoint memory
roots remain independent. Terminators and fixed-register/clobber boundaries
remain explicit.

Each block receives a boundary contract produced by liveness and the
materialization plan:

```text
mandatory live-ins
mandatory live-outs
rematerializable live-ins
reload-at-entry values
store-before-exit values
per-register-class budgets
```

The scheduler may reduce local pressure within that contract. It cannot repair
an infeasible cross-block carry or silently choose a new home.

The replacement scheduler is bidirectional:

- the top queue contains operations whose operands are available;
- the bottom queue contains operations whose users/effects are placed;
- candidate selection tracks exact pressure deltas, critical path, target
  constraints, and rematerialization cost.

Starting from a final Store on the bottom side naturally completes its operand
cone immediately before the Store. This is structurally different from adding
an exit-value priority to the current bottom-up queue: the dependency root,
complete backward cone, and top/bottom frontiers are explicit.

The scheduler must operate in `O(I log I + E)` time and `O(I + E)` space for a
block. No operation scans the complete ready set, complete live set, or all
VRegs per scheduled instruction.

## Physical allocation contract

Physical allocation consumes a closed `UseClusterPlan`; it does not postpone
the carry/rematerialize/home decision until after Store deletion. For one
logical range and use cluster, every source is already one of:

- a planned carried SSA value;
- an explicit pure rematerialization recipe;
- an exact load from a preserved Store materialization;
- an exact load from an explicit private scratch Store.

A logical StateVersion is never itself a home. One long VReg is not the
representation of an entire promoted RTL range.

Stable instruction and edge-use identities are required so a placement or
schedule change does not silently invalidate the plan. Reload recipes retain
the exact MemorySSA version at their insertion point. Reconstruction may not
invent an unplanned reload, spill, edge copy, or scratch register.

The production W/S spill planner and existing reload-recipe analysis already
demonstrate point/edge MemorySSA recipe semantics for concrete loads. They do
not supply the missing materialization planner. The open work is to construct
use-cluster plans before destructive Store/Load removal, preserve or introduce
every selected materialization site explicitly, and make placement,
scheduling, and physical allocation consume the same plan.

## Memory and compile-time discipline

The current fused pre-optimized SIR text exceeds 58 MB. The implementation
must not keep multiple complete cloned functions during analysis.

Required discipline:

- immutable stable IDs into one input function;
- compact per-block instruction tables;
- sparse def/use, range, and CFG side tables;
- one atomic rewrite plan;
- at most one output function constructed during commit;
- no all-pairs value graph or dense instruction-by-value matrix;
- explicit accounting of materialized range-graph size `R`;
- no bit-width-proportional state representation.

The existing exact promotion helper clones the complete EU for preview and
again for rewrite. That strategy is not acceptable for the complete
range-aware pass and must not be copied.

## Feasibility assessment

| Component | Evidence already present | Assessment |
|---|---|---|
| Fused event boundary | Native emission already merges comb and one FF event and records the suffix entry | Feasible |
| Sparse CFG, dominance, SCCs | Shared `celox-analysis` CFG infrastructure is used by production passes | Feasible |
| Access-boundary range indexing | Unit-independent exact interval indexing exists with `O(N log N)` construction | Feasible; eager overlap expansion is rejected |
| State versioning | Exact-fragment StateSSA and native MemorySSA already build def/use/phi versions | Feasible, but lazy/persistent range composition and `R` bounds are new |
| Sink-rooted DCE | Address-rooted DSE, register DCE, and dead-control-region elimination exist separately | Feasible after roots are unified |
| Single-use Store placement | Dominance, post-dominance, use collection, and guarded sinking exist | Feasible |
| General early/late placement | Required analyses exist, but there is no complete production GCM pass | Moderate implementation risk |
| Concrete preserved-state reloads | Point- and edge-specific MemorySSA reload recipes are production features | Feasible only while the defining Store remains |
| Materialization planning | No production phase currently chooses carry/rematerialize/preserved/scratch sources before promotion | Highest design and integration risk |
| Use-local splitting | Production W/S planning supports point/edge operations; the alternative interval path documents broader splitting but is not the production authority | High integration risk |
| Bidirectional scheduler | Sparse dependency tracking exists; current scheduler is one-sided and must be replaced rather than extended | Moderate implementation risk |
| Dynamic indexed range coverage | Bounded effects exist for selected native operations, but complete SIR range proofs do not | High coverage risk; conservative fallback required |
| Four-state aggregate promotion | Value/mask atomicity is represented, but complete mixed-range promotion is not proved | High semantic risk; defer |

### Verdict

The static, two-state fused specialization is implementable with existing CFG,
StateSSA, interval, MemorySSA, and reload-recipe foundations. It does not
require a parser rewrite or an arbitrary graph partitioner.

The performance result is not yet proved. The design is worth implementing
only if the early analysis shows that the FF/event roots make a large fraction
of hot packed comb Store/Load and extract/merge work removable. If the static
admissible subset covers only a small fraction of the complete fused function,
the project must stop and improve range/alias coverage before changing the
scheduler or allocator.

The largest technical risk is not ADCE. A StateVersion has no inherent home.
Before Store deletion, the compiler must choose a concrete materialization for
every use cluster, then preserve that choice through GCM, scheduling, plan
repair, and physical allocation. Enabling cross-phase forwarding without that
contract has already failed.

## Execution plan and stop conditions

### Milestone 0: analysis-only coverage

Build lazy or persistent range versions, semantic roots, and backward liveness
without rewriting SIR. This is the only milestone approved before its measured
result and the Milestone 1 type contract are reviewed.

Required output for the complete fused function:

- admitted and rejected objects/ranges with structural reasons;
- total logical segments;
- total MemoryDef fragments;
- total access/fragment overlap edges;
- total MemoryPhi operands;
- maximum fragments touched by one access;
- maximum versions for one object;
- static counts of candidate removable Store, Load, extract/merge, and producer
  instructions;
- candidate instructions attributed to packed address calculation, range
  extraction, range insertion, mask generation, memory traffic, mux lowering,
  or unrelated arithmetic;
- the percentage of profiled zero-extending loads, shifts, mask `and`
  operations, and stores attributable to candidate removable work;
- analysis time and peak resident memory;
- verifier result for every reaching version and root.

Stop if:

- construction is not `O(N + R + E)` apart from endpoint sorting;
- `R` shows quadratic access/fragment or segment/join expansion on the
  complete workload or adversarial focused fixtures;
- resident memory scales with declared bit width or an instruction/value
  product;
- the admitted hot range set is too small to explain a substantial part of
  the measured packed-work gap.

This milestone must leave generated SIR and MIR byte-identical.

### Milestone 1: materialization and FF phase model

Before any Store deletion, define and independently verify:

- `StateVersion` and executable defining recipes;
- `MaterializationSite` identities;
- `UseClusterPlan` source and exit actions;
- movable `StageNextFF` effects;
- the fixed `CommitFFState` phase barrier;
- block boundary contracts;
- the finite monotonic plan-repair relation.

Run the model on Milestone 0 facts without rewriting generated SIR. Every
retained use must have one concrete source, and deleting a preserved Store
must invalidate every plan which names that Store.

Stop if planning can produce an implicit home, whole-function mandatory VReg,
untyped FF Store/commit effect, or repair cycle.

This milestone also requires review before code-generating work begins.

### Milestone 2: atomic range promotion

On focused fixtures, replace admitted comb Store/Load chains with range SSA
values according to a complete materialization plan. Preserved state and
private scratch homes remain explicit operations. Apply one verified plan to
a new function.

Tests include:

- straight-line full/narrow overlap;
- partial overwrite;
- diamond and loop MemoryPhi;
- unchanged predecessor edge;
- multiple FF sinks;
- effect, phase, dynamic-alias, and four-state rejection.

Stop if a replacement requires one whole-function live range, treats a
StateVersion as a home, or loses a planned materialization site.

### Milestone 3: sink-rooted ADCE

Unify register, state-version, effect, and control roots. Remove unreachable
comb stores and computations only in the fused clone.

Compare the original and specialized fused functions on randomized small
state, branch, loop, and partial-write fixtures. Keep the generic paths as the
independent reference.

Stop if capture, trigger, simultaneous FF sampling, cascade, or sparse commit
semantics cannot be represented as explicit roots/barriers.

### Milestone 4: plan-constrained placement and repair

Implement placement first for pure single-use cones, then shared cones under
the selected carry/rematerialize/preserved/scratch plan. Placement is accepted
only when an independent dominance, MemorySSA-version, loop-frequency, FF
phase, and effect verifier succeeds.

Inspect the complete optimized SIR to prove that promoted producer cones move
next to their Store/FF sinks instead of merely increasing live range length.
Run local pressure validation and apply only the specified monotonic repair
operations until the plan validates or falls back to an explicit
materialization.

### Milestone 5: scheduling and physical allocation integration

Introduce stable machine instruction identities and the bidirectional
per-block scheduler. Feed it explicit residency/rematerialization/reload
choices and block boundary contracts; do not add another scalar priority to
the current scheduler.

Verify:

- sparse dependency order;
- fixed-register and clobber constraints;
- reconstructed pressure;
- exact point/edge reloads and their concrete defining materializations;
- final SSA assignment and destruction.

### Milestone 6: production and performance gate

For every code-generating milestone run the focused suites, common non-LTO
tests, complete SIR/MIR inspection, and exact Heliodor Linux workload described
in the native throughput plan.

Acceptance requires:

- normal power-down at exactly `cy=9ae070 x3=aa pass=1`;
- separately reported code-generation and generated execution time;
- a substantial reduction in the identified packed memory/extract work;
- generated execution improvement larger than run-to-run noise;
- no unbounded compile-time or resident-memory regression.

Only the complete retained design receives one final release/LTO
qualification.

## Rejected alternatives

### Parser expression DAG

Procedural ordering, partial writes, dynamic indexing, loops, hierarchy, and
effects would require rebuilding CFG, SSA, MemorySSA, and alias analysis in
the parser. The already verified SIR is the correct starting point.

### Whole-function scheduling DAG

It conflates global code placement, register pressure, and machine scheduling,
and does not scale to the measured function.

### Arbitrary allocation-region partitioning

No sound, practical partition objective has been established for the shared
multi-sink graph. Existing CFG blocks, dominance, loops, and sparse live-range
splitting are the supported decomposition.

### Unconditional cross-phase forwarding

It removes memory syntax before providing use-local homes and has already
created long live ranges and spill regressions.

### Existing ready-queue priorities

Store priority, exit-value priority, and aggregate pressure scores do not
perform dominance-safe code placement and do not represent allocator-selected
split regions.

## Related documents

- [Native throughput execution plan](./native-throughput-execution-plan.md)
- [Native register allocation](./native-register-allocation.md)
- [Simulator architecture](./architecture.md)
- [Branch-aware mux lowering](./branch-aware-mux-lowering.md)
