# Fused state SSA and code placement

> **Status:** Milestone 0 and the analysis-only Milestone 1 contract are
> complete. Milestone 1 fails its profile-weighted profitability gate, so
> Milestone 2 code generation is not authorized. Each future code-generating
> milestone still requires an independent semantic, generated-code, and Linux
> execution gate.

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
- **MaterializationLeaf** is an executable, phase-correct source at one use
  cluster. Merely proving that a StateVersion exists is not a leaf.
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

A stage has an explicit placement window. Its earliest point is after its
value, guard, and StateSSA dependencies; its latest point is before the fixed
commit barrier. Dependencies preserve overlapping/partial-write priority,
branch guards, old-state versus staging aliases, trigger/capture/runtime
observations, and the final range StateSSA edge. `CommitFFState` is one fixed
phase barrier which atomically publishes each FF range's final StateVersion.
Normal FF reads cannot observe staging storage before that barrier.

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

Every rematerialization frontier leaf is exactly one of:

```text
Constant(value)
DominatingSSA(value_id, insertion_point)
ReloadPreservedHome(site_id, state_version_id)
ReadPersistentState(object, range, phase_version)
ControlMerge(recipe)
```

`DominatingSSA` must dominate the insertion point. A preserved-home reload
names the exact Store site and StateVersion, and that version must reach the
reload point. A persistent-state read names the FF/input snapshot phase.
`ControlMerge` reproduces branch priority and control dependence. No recipe
may contain an effectful operation or an unhandled loop-carried cycle.

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

The initial `KeepPackedReload` form is itself concrete:

```text
KeepPackedReload {
    original_load_site
    preserved_store_sites
    state_versions
    extract_merge_recipe
}
```

Store invalidation follows these identities and versions, not just an address.

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

The initial source-action order is
`DirectForward -> Rematerialize -> KeepPackedReload`, with the direct-to-packed
edge also allowed. Cluster repair only refines the partition of the finite
original use-site set and never rejoins split clusters. The lexicographic rank
`(unsplit use pairs, direct plans, rematerialize plans)` strictly decreases.

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

Cluster-local rematerializations retain distinct stable identities. GVN/CSE
may not merge them across clusters unless a new `DirectForward` boundary
contract is constructed and verified.

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

Milestone 1 can establish only a boundary-feasible `DirectForward`. Its
contract records traversed CFG edges, register class, mandatory live-in/out
contributions, producer/use identities, and the legal placement interval.
Milestone 4 scheduling must revalidate block-internal temporaries; failure
monotonically repairs the cluster to rematerialization or packed reload.

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

#### Milestone 0 result

The complete Heliodor fused function was analyzed with semantic exit roots and
FF range demands. A representative run produced:

| Measure | Result |
|---|---:|
| SIR blocks / instructions | 14,138 / 103,336 |
| Static accesses / logical segments | 32,476 / 22,978 |
| MemoryDef fragments / overlap edges | 26,618 / 37,734 |
| MemoryPhi operands | 7,281 |
| Largest access fragmentation | 38 |
| Largest object version set | 475 |
| FF demands / admitted FF loads | 3,666 / 2,485 |
| Candidate backing stores | 1,282 |
| Immediately removable backing stores | 0 |
| Cumulative resolved range fragments | 330,172 |
| Admitted / partial / rejected objects | 942 / 2 / 261 |
| Analysis time | 1.79 s |
| RSS before / after analysis | 4,685,516 / 4,751,852 KiB |
| Process peak RSS | 4,751,852 KiB |

All 3,666 demands and 3,187 noncandidate/exit roots were independently
resolved and verified. The candidate backing stores remain required by the
final public state, so the result specifically supports forwarding promoted
values to FF uses while retaining final stores; it rejects a store-deletion
only implementation.

`resolved_range_fragments` is the cumulative number of fragments returned by
memoized range-resolution queries. It is neither a simultaneously resident
fragment count nor a count of retained rewrite candidates. Resident memory is
reported separately above; the analysis increased RSS by about 65 MiB in the
isolated run.

Instruction sampling used 10,219 samples in `eval_comb_apply_ff`. Mapping each
sample to the final emitted instruction and to the candidate producer block
gave:

| Emitted operation | All samples | Candidate-block samples | Share |
|---|---:|---:|---:|
| zero-extending memory load | 346 | 217 | 62.7% |
| shift | 1,062 | 587 | 55.3% |
| mask `and` | 1,628 | 676 | 41.5% |
| memory store | 855 | 443 | 51.8% |
| all emitted operations | 10,219 | 3,638 | 35.6% |

Block attribution is deliberately conservative when a block contains both a
candidate cone and unrelated work. It is nevertheless large enough to pass
the Milestone 0 structural-coverage condition. It did **not** prove removable
work: those producer blocks include the public backing Stores which the
Milestone 1 contract must preserve. The profile-weighted Milestone 1 result
below supersedes this diagnostic attribution for the profitability decision.

With analysis disabled and enabled, SHA-256 hashes were identical for all
four complete outputs: pre-optimized SIR, post-optimized SIR,
native-optimized SIR, and final MIR. The persistent range representation was
also exercised by adversarial overlap fixtures and did not eagerly construct
the quadratic overlap graph.

### Milestone 1: materialization and FF phase model

Before any Store deletion, define and independently verify:

- `StateVersion` and executable defining recipes;
- `MaterializationSite` identities;
- `UseClusterPlan` source and exit actions;
- movable `StageNextFF` effects;
- the fixed `CommitFFState` phase barrier;
- block boundary contracts;
- the finite monotonic plan-repair relation.

The measured Milestone 0 result narrows the initial source choices for each FF
range demand to:

```text
DirectForward
Rematerialize
KeepPackedReload
```

All public backing stores remain. `KeepPackedReload` is therefore a concrete,
always-correct fallback, not a failed promotion. The initial model does not
introduce private scratch. Direct forwarding is admitted only after its block
boundary pressure contract validates; rematerialization accepts only an
executable pure cone. Different use clusters of one StateVersion may select
different choices.

The first strict-pure-cone run is a diagnostic baseline, not a profitability
pass:

| Initial plan | FF range demands |
|---|---:|
| `DirectForward` | 0 |
| `Rematerialize` | 36 |
| `KeepPackedReload` | 2,449 |

The exclusive fallback reasons were 112 unsupported MemoryPhi/control-merge
recipes, 2,319 producer cones containing a non-pure frontier, and 18 cases where
rematerialization was estimated to cost more than the packed reload. The
non-exclusive predicate order is:

```text
unsupported-memory-phi, unsupported-recipe, non-pure-producer,
no-legal-placement, cone-over-16-instructions, shared-producer,
cross-block-direct-range, rematerialization-more-expensive
```

This result does not pass the profitability gate. It shows that requiring the
complete producer cone to be pure is the dominant rejection. The next
analysis slice must introduce an explicit materialization frontier and prove
the version available at each frontier leaf; it must not weaken purity by
assumption.

The subsequent executable-frontier analysis stops a producer walk only at a
target-local dominating SSA value or at a static persistent-state Load whose
exact range StateVersion is independently proved equal at the original and
target points. It produced:

| Frontier plan | FF range demands |
|---|---:|
| boundary-feasible `DirectForward` (closed-cone relocation) | 228 |
| partial `Rematerialize` | 255 |
| `KeepPackedReload` | 2,002 |

Thus 483 of 2,485 candidate packed reloads (19.4%), or 13.2% of all 3,666 FF
demands, have an executable non-packed source. These are static counts, not a
speedup claim. The remaining exclusive fallback reasons are 112 unsupported
MemoryPhi/control merges, 794 unsupported frontier recipes, 263 genuinely
non-pure producers, 130 unstable range versions, and 703 cases estimated more
expensive than the packed reload.

On the same 103,336-instruction workload, this frontier run took 2.79 seconds.
RSS increased from 7,707,216 to 7,844,664 KiB (137,448 KiB, about 134 MiB).
The model build, base verification, reverse Store-dependency audit, and summary
all remained at 7,838,136 KiB in that run. Two accidental nonsparse costs were
removed before accepting this result:

- Store invalidation no longer re-verifies the complete model once per Store;
  it constructs one reverse dependency index and audits each named Store by
  identity.
- per-value cone queries no longer allocate the complete dominator path merely
  to test one target placement.

Mapping the existing 10,216 `eval_comb_apply_ff` instruction samples to final
emitted blocks gives:

| Profile scope | Samples | Share |
|---|---:|---:|
| any block containing one of the 2,485 candidate FF reloads | 252 | 2.47% |
| block containing a `DirectForward` or `Rematerialize` plan | 81 | 0.79% |

The operation-specific block upper bounds are:

| Emitted operation | All samples | Any candidate-use block | Executable-plan block |
|---|---:|---:|---:|
| zero-extending load | 344 | 15 (4.36%) | 6 (1.74%) |
| shift | 836 | 21 (2.51%) | 3 (0.36%) |
| mask `and` | 1,628 | 28 (1.72%) | 6 (0.37%) |

These are deliberately upper bounds because a block may contain unrelated
instructions. Even transforming every currently admitted FF reload cannot
explain the native/Veryl gap while all public backing Stores remain. The
earlier 35.6% producer-block attribution measured where candidate values were
computed, not work removable by this plan.

Two code-generation probes independently confirmed this gate:

| Probe | final Loads | final Concats | fused x86 instructions | hot `b4212` x86 instructions |
|---|---:|---:|---:|---:|
| current baseline | 21,421 | 5,789 | 152,614 | 1,777 |
| forward every locally contained Load as a Slice | 20,917 | 5,759 | 152,230 | 1,776 |
| fold only adjacent Store/split-Load/Concat round trips | 21,371 | 5,757 | 152,609 | 1,777 |

The broad probe removes 504 Loads but only 384 final x86 instructions (0.25%);
the hottest large block shrinks by one instruction. Restricting the rewrite to
a complete adjacent round trip removes 50 Loads and 32 Concats but only five
final x86 instructions. A native narrow packed Load can be cheaper than a
shift/mask, while carrying its source to a later use can increase live-range
pressure. Static Load or Concat deletion is therefore not evidence of
profitability. The measured reduction remains far too small to overturn the
profile gate above. The Store and its packed reload may be removed together
only under a plan which proves the Store unobservable; otherwise
`KeepPackedReload` remains an intentional materialization choice.

The broad contained-Load rewrite is retained as a separately justified local
mem2reg improvement: it uses an exact containing Store range, preserves
overlap invalidation, and materializes the requested subrange with a `Slice`.
It does not authorize Milestone 2 or Store deletion. Two unchanged-workload
Linux runs reached the exact baseline marker
`cy=9ae070 x3=aa pass=1` in 65.002 and 66.484 seconds of execution, versus a
recent 70.010-second baseline run. This is evidence of no observed runtime
regression, not a claimed 6% improvement: the historical run-to-run variation
overlaps the difference. The adjacent round-trip probe is not retained.

Milestone 1 therefore fails the profitability gate. Milestone 2 must not start
from this plan. A future restart requires either:

- a stronger fused-call observability contract which makes a measured hot set
  of public Stores genuinely dead; or
- new profile evidence showing that FF reload/extract work itself has become a
  dominant cost.

Until then, optimization work should target the independently hot
combinational control/dataflow and machine scheduling/allocation paths.

### Post-gate hot-path follow-up

A fresh `cycles:u` profile of the retained contained-Load build reached the
exact Linux marker and collected 172,457 samples. The profile was regenerated
because even a local rewrite changes JIT block placement; block identities from
an older perf map were not reused.

The profile still contains high-density counted-loop bodies. A representative
post-RA body formed an element address as:

```text
mov index
shl 6
shr 3
load [state + offset + index]
```

There are 130 static `shl 6; shr 3; state-memory operation` sequences in the
fused function. Inspection of the corresponding SIR showed that the
representative index is already a 32-bit value. The first rejection was not a
missing loop-bound proof: the element offset retained a
`dynamic_bit_offset=Some(zero_register)`, although that register has the exact
constant value zero. Allowing this executable constant fact in direct element
address lowering converts 28 sequences to direct byte-index computation and
reduces fused x86 instructions from 152,230 to 152,198. The complete Linux run
still ends at `cy=9ae070 x3=aa pass=1`.

The remaining 102 `shl 6; shr 3` sequences must be classified by their actual
dynamic-offset recipes before adding a general range analysis. Independently,
direct byte indices which are already proven safe now carry an explicit x86
address scale. A single-use `ShlImm` by one, two, or three is folded into
`LoadIndexed` scale 2, 4, or 8. This is an exact machine rewrite: both the
64-bit shift and x86 effective-address multiplication wrap modulo 64 bits. It
does not reinterpret the earlier RTL bit-offset expression.

On the complete fused function, scaled addressing changes:

| Stage | Before scale | With scale | Delta |
|---|---:|---:|---:|
| optimized MIR | 96,767 | 96,561 | -206 |
| pressure-scheduled MIR | 103,700 | 103,494 | -206 |
| post-RA MIR | 225,868 | 225,462 | -406 |
| x86 instructions | 152,198 | 151,872 | -326 |

The representative hot loop now performs
`load [state + index*8 + offset]` directly. All 490 native-backend tests pass,
and the complete Linux workload reaches `cy=9ae070 x3=aa pass=1` with
64.018 seconds of generated execution. The immediately preceding run took
64.152 seconds, so the timing difference is treated as noise; the retained
result is the structural removal of address temporaries and instructions.

The next structural inspection found fixed state copies which had already been
scalarized into adjacent direct `Load`/`Store` pairs before allocation. This
needlessly gives each copied machine word a VReg and hides the bulk operation
from emission. A late MIR idiom pass now reconstructs `MemCopy` only when:

- every loaded value is used exactly once by its adjacent matching Store;
- all source and destination chunks are contiguous and equal-width;
- the complete source and destination ranges are disjoint; and
- the reconstructed copy is at least 16 bytes.

Nonoverlapping copies use 16-byte machine moves with exact scalar tails;
overlapping copies retain the existing ordered implementation. On the fused
function this recovers 24 copies, including the two 64-byte copies in hot block
`b4212`, and changes:

| Stage | Before copy recovery | With copy recovery | Delta |
|---|---:|---:|---:|
| optimized MIR | 96,561 | 96,131 | -430 |
| pressure-scheduled MIR | 103,494 | 103,064 | -430 |
| post-RA MIR | 225,462 | 224,681 | -781 |
| x86 instructions | 151,872 | 151,596 | -276 |

All 493 native-backend tests pass. The complete Linux workload reaches the
exact `cy=9ae070 x3=aa pass=1` marker with 72.017 seconds of code generation
and 65.280 seconds of generated execution. This run does not establish a
timing improvement over the preceding 64.018-second run; the retained result
is the removal of artificial allocation values and scalar copy instructions.

The first larger post-gate improvement comes from strengthening the fused-call
observability contract, not from forwarding more FF reloads. The only caller,
`tick_deferred_comb`, sets `dirty = true` immediately after
`eval_comb_apply_ff` returns. Every external signal read then runs the ordinary
`eval_comb` before reading simulator state. Consequently, a comb-prefix Store
in the fused clone is not an exit root merely because the standalone
combinational function publishes the same signal.

The first production subset removed a comb-prefix Store only when all of the
following held:

- it is a static two-state STABLE Store outside the FF suffix;
- its trigger and capture sets are empty;
- no static Load, Commit source, or effectful Store in the complete fused
  function reads an overlapping bit range; and
- the same object has no dynamic Load or Store.

FF-suffix Stores remain persistent-state publications. That subset was
intentionally more conservative than full range MemorySSA DSE: even a Load
which occurred only before the Store kept it live. It nevertheless removed 321
fused SIR Stores and their now-dead producer cones. The complete function
changed:

| Stage | Before dirty-exit DSE | With dirty-exit DSE | Delta |
|---|---:|---:|---:|
| optimized MIR | 96,131 | 94,305 | -1,826 |
| pressure-scheduled MIR | 103,064 | 101,267 | -1,797 |
| post-RA MIR | 224,681 | 220,726 | -3,955 |
| x86 instructions | 151,596 | 149,327 | -2,269 |

The native backend's 493 tests and the high-level native-testbench, native
execution, counter, FF, dynamic-NBA, and cross-block-NBA suites pass. Two
complete Linux runs both reached `cy=9ae070 x3=aa pass=1`; code generation took
70.116 and 70.841 seconds, while generated execution took 61.912 and 64.696
seconds. The structural reduction is established. The timing mean is lower
than the preceding 64--67 second profile runs, but the overlap remains too
large to attribute the complete difference to this pass.

The production pass now replaces that whole-function read test with the same
sparse object MemorySSA and range resolver used by the feasibility analysis.
Its roots are exact reaching versions for Loads and Commit sources, incoming
versions observed by trigger/capture Stores, and complete exit versions only
for objects with an FF-suffix publication. Dynamic accesses conservatively
retain every candidate Store on their object. Per-object candidate and
definition indices avoid rescanning all Stores for every object.

This removes another 24 SIR Stores whose overlapping reads observe an earlier
version. Relative to the first subset:

| Stage | Any-read subset | MemorySSA liveness | Delta |
|---|---:|---:|---:|
| optimized MIR | 94,305 | 94,201 | -104 |
| pressure-scheduled MIR | 101,267 | 101,169 | -98 |
| post-RA MIR | 220,726 | 220,551 | -175 |
| x86 instructions | 149,327 | 149,218 | -109 |

The MemorySSA version passes five focused range/version/effect/publication
tests, all 493 native-backend tests, native testbench 60/60, FF 200/200,
dynamic NBA 33/33, and cross-block NBA 11/11. The rebuilt complete Linux
workload reaches `cy=9ae070 x3=aa pass=1` with 72.373 seconds of code generation
and 64.031 seconds of generated execution.

This is a conservative Milestone 3 subset and does not authorize the failed
Milestone 1 FF materialization plan. Its small incremental reduction also
shows that ordering-insensitive liveness was not the main remaining source of
packed RMW work; the retained hot Stores feed real later versions and require
Store/load elimination or use-local promotion rather than more exit DSE.

An implementation probe then applied exactly that use-local policy to internal
comb Store/load round trips. It required one direct MemorySSA definition,
replaced every direct Load user of the Store, kept every materialization inside
the Load block, and reran dirty-exit DSE. No value was carried across a CFG
edge. Three progressively stricter subsets produced:

| Use-local probe | Replaced Loads | optimized MIR | post-RA MIR | x86 instructions |
|---|---:|---:|---:|---:|
| executable partial frontier | 234 | 93,857 | 220,684 | 149,237 |
| no `DominatingSSA` frontier | 210 | 93,922 | 220,806 | 149,244 |
| one phase-stable state-read leaf, empty pure suffix | 174 | 93,984 | 221,033 | 149,348 |
| retained MemorySSA-DSE baseline | 0 | 94,201 | 220,551 | 149,218 |

All three probes improve pre-allocation instruction count and all three regress
after allocation. Even the final subset is structurally allocation-neutral at
the use: it changes a Store followed by a later Load into a phase-equivalent
Load from the original persistent state object, without cloning arithmetic or
extending an SSA value across blocks. Nevertheless it adds 482 post-RA
instructions and 130 final x86 instructions. The probe implementation is not
retained.

This invalidates the current profitability contract, not the MemorySSA
legality proof. SIR expression cost and block-local placement are insufficient:
removing an explicit memory home changes the allocator's global home,
reconstruction, phi/copy, and spill decisions. Milestone 2 must therefore
remain stopped until the allocation pipeline can expose and validate a stable
allocation-region contract. At minimum, a candidate plan must predict and then
verify:

```text
boundary live-in/live-out delta by register class
new reconstruction phi and edge-copy obligations
removed and introduced explicit homes
reload recipe availability at every use
spill/reload delta inside the affected allocation regions
```

A failed contract repairs monotonically back to `KeepPackedReload`. Reordering
SIR register identities or deleting a home must not be allowed to silently
select a globally different, more expensive reconstruction plan. Re-enabling
use-local code generation before this interface exists would merely move the
performance failure from SIR into register allocation.

The allocator audit then found an older transformation which violated this
contract directly. `forward_state_round_trips` ran *after* pressure scheduling
and replaced thousands of MemorySSA-proved state Loads with cross-block `Mov`
affinities. The original Store remained in MIR, but production never assigned
the advertised `deferred_state_home`: every assignment of that descriptor was
test-only. The transformation therefore changed the live-range graph after the
scheduler had fixed instruction order, without providing the allocator-owned
home promised by its comment.

Removing this late forwarding path, while retaining local post-schedule memory
folds, changes the same fused function as follows:

| Stage | Late state forwarding | No late forwarding | Delta |
|---|---:|---:|---:|
| optimized MIR | 94,201 | 94,201 | 0 |
| post-RA MIR | 220,551 | 213,542 | -7,009 |
| x86 instructions | 149,218 | 148,370 | -848 |
| spill frame | 4,688 bytes | 4,336 bytes | -352 bytes |
| post-allocation blocks | 26,224 | 25,989 | -235 |

This is the first allocation result in this investigation which improves the
actual downstream representation rather than only pre-RA SIR/MIR. It also
explains why the use-local Store-deletion probes were unstable: they were
measured on top of a second, unplanned global forwarding pass which changed the
same memory/value boundary after scheduling. State forwarding may return only
as a plan-constrained transformation before pressure scheduling, or after
scheduling with an independently verified no-pressure-delta contract and a
concrete reload home. A raw MemorySSA reaching-definition proof is not enough.

The native backend's 478 remaining tests pass after deleting the unused
forwarding implementation. Native testbench 60/60, native execution 16/16, FF
200/200, dynamic NBA 33/33, and cross-block NBA 11/11 also pass. The complete
Linux workload reaches the exact `cy=9ae070 x3=aa pass=1` marker; code
generation takes 52.738 seconds and generated execution takes 64.973 seconds.
The structural allocation reduction is established, but this single runtime
sample does not establish a throughput improvement over the noisy 61.9--64.7
second range measured immediately before it.

The distance histogram bins are `0`, `1`, `2..3`, `4..7`, `8..15`, `16+`,
and unresolved. Cone-size bins are `0`, `1..2`, `3..4`, `5..8`, `9..16`,
`17..32`, and `33+`. Version-demand bins are `1`, `2`, `3..4`, `5..8`,
`9..16`, and `17+`.

The analysis distinguishes effect units in its output:

- `stage_next_ff_sites` counts movable staged-write sites;
- `commit_ff_state_dependencies` counts range publication dependencies on the
  fixed suffix barrier, not barrier instructions.

Run the model on Milestone 0 facts without rewriting generated SIR. Every
retained use must have one concrete source, and deleting a preserved Store
must invalidate every plan which names that Store.

Stop if planning can produce an implicit home, whole-function mandatory VReg,
untyped FF Store/commit effect, or repair cycle.

#### Milestone 1 result

Every non-packed plan has an executable, phase-correct frontier source. The
model contains 228 target-local closed-cone `DirectForward` contracts. Each
records the original producer/use identity, native 32-bit, 64-bit, or
multi-chunk GPR class, target-only legal placement interval, and empty
cross-block live-in/live-out contribution. Carry-style forwarding is not
silently inferred.

The concrete repair relation implements the lexicographic rank
`(unsplit use pairs, direct plans, rematerialize plans)`. Cluster repair only
refines a partition; tests reject rejoining a split cluster and every reverse
source-action transition.

The reverse Store dependency index names exact StateVersion/materialization
dependents and validates all 1,282 preserved Stores without repeatedly walking
the complete model.

The semantic/type contract passes, but the profile-weighted profitability gate
above fails. Code-generating Milestone 2 is stopped.

### Milestone 2: use-local FF forwarding

On focused fixtures, bypass admitted FF Load/extract chains according to a
complete materialization plan while preserving every public backing Store.
Apply one verified plan to a new function. No Load is promoted implicitly when
its use cluster lacks a plan.

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
