# Decision-region architecture

This document freezes the control and pressure architecture that follows the
verified SSA allocator and the first binary mux lowering. It is a phase design,
not a list of independent peepholes. Implementations must preserve this order
and must not recover from a failed proof with a legacy path, retry loop, or
size/count cutoff.

## Why the binary transform is not enough

Celox has never completed the pinned Heliodor Linux-boot gate at a competitive
speed. Historical status-zero records were compile-only, and the timings below
are explicitly partial-run diagnostics rather than a fast end-to-end baseline.

On the pinned Heliodor Linux-boot input, source-DAG lowering observes 22,344
muxes. It converts 2,579 to binary CFG, correctly keeps 16,509 whose local
expected cost is unfavorable, and rejects 3,227 profitable candidates only
because a nontrivial expression is shared below the two arm roots. Only one
shared node is currently hoisted. The resulting `eval_comb` still contains a
5,545-instruction straight region, has measured pressure 2,229 before and 2,024
after scheduling, and allocates a 79,216-byte spill frame.

The Veryl AOT comparison evaluates all 31 combinational chunks every tick, so
event-driven execution is not the explanation. The measured GCC output
contains 159 indirect jump-table dispatches, balanced searches, and
branch/cmov hybrids; this is consistent with multiway recovery from the
generated equality chains, though the compiler's internal recognition trace
was not captured. Its 31 functions
also confine each stack frame to at most about 1,464 bytes. Celox therefore
needs explicit control regions, correct placement of shared data nodes, and
verified pressure boundaries. Changing a per-mux threshold cannot provide any
of those properties.

## Fixed phase order

```text
symbolic evaluation with source control provenance
  -> checked SLTNodeFacts, SLTVersionTable, and ControlProvenance verification
  -> ExecutionSafetyAnalysis and ControlEligibilityPlan verification
  -> maximal ControlSkeleton legality verification
  -> ScheduleEarly / ScheduleLate DAG placement
  -> bottom-up gate/decision selection
  -> rejected-region contraction
  -> one final DAG placement
  -> GateFormationPlan and DecisionFormationPlan input verification
  -> SIR construction, formation-output relation, and DecisionRegion verification
  -> target-independent SIR optimization and DecisionRegion re-verification
  -> target DecisionLoweringPlan verification
  -> instruction selection with explicit native MDecision
  -> target decision legalization and LoweredDecisionWitness output verification
  -> predicate-aware SLP/store combining
  -> semantic pressure-frontier block splitting and CFG verification
  -> pressure-aware MIR scheduling
  -> MIRMemoryVersionAnalysis, CSSA, next-use, and loop analysis
  -> PressureRegion and spill planning together
  -> SSA reconstruction, pressure/home/Perm verification
  -> final phi-congruence classification and affinity coloring
  -> SSA destruction
  -> copy/probability-aware block layout verification
  -> emission
```

The techniques solve different problems:

| Phase | Responsibility | It must not do |
| --- | --- | --- |
| Control provenance | retain source `if`, `case`, and ternary predicates before symbolic merging loses them | decide target profitability |
| DAG placement | place each pure `InstValue` once at the latest legal site inside its execution-safety domain | invent or remove control |
| Gate selection | choose eager dataflow versus control using expected target work | clone a global cache per arm |
| Decision lowering | choose value table, jump table, search tree, ordered chain, or branchless tail | repair invalid or overlapping semantics |
| PressureRegion planning | choose verified full-register cuts, then constrain the one ordinary spill plan | split into functions or retry allocation |
| Component affinity | reduce copies after feasibility is proved | force an unavailable common color |
| Block layout | choose fallthroughs after copies are known | change CFG edges or SSA semantics |

## 1. Source control provenance

Recovering predicates from an arbitrary hash-consed mux DAG is not the primary
algorithm. A shared node may be reached under many syntactic contexts, and
expanding those contexts or repeatedly recomputing their LCA can be nonlinear.
Symbolic evaluation must instead retain the source control tree when it creates
the muxes.

The SLT arena gains a serialized `ControlProvenance` side table. An arena may
hold several combinational declarations and, after flattening, several module
instances, so provenance is a forest of isolated control units rather than one
arena-wide tree:

```text
ControlUnitId, ExternalRootId, GateId, DecisionId, GatedMuxId,
DecisionResultMergeId, PredicateRegionId, ControlPointId, InstValueId:
checked u32 IDs

ControlProvenance
  units / regions / points / gates / decisions
  gated_muxes / decision_result_merges / inst_values
  nonserialized canonical caches

ControlUnit
  root_region: PredicateRegionId
  entry / exit: ControlPointId
  scheduled_roots: [ControlRoot]

ControlRoot
  external_root: ExternalRootId
  kind: LogicPath | Observer | RuntimeEvent
  ordered operands: [(RootOperandRole, InstValueId)]
  effect_action: optional (ControlPointId, action_index: usize)
  use_site: ControlSite

PredicateRegion
  unit: ControlUnitId
  parent: optional PredicateRegionId
  entry / exit: ControlPointId
  owner: Root | GateTrue(GateId) | GateFalse(GateId)
       | DecisionArm(DecisionId, arm) | DecisionDefault(DecisionId)

ControlPoint
  unit: ControlUnitId
  region: PredicateRegionId
  kind: UnitEntry | ArmEntry | RegionExit | GateHeader
      | DecisionHeader | Join | Continuation | Effect
  ordered actions
  predecessor / successor ControlPointIds

Gate
  unit: ControlUnitId
  parent_region
  condition: InstValueId
  header / true_region / false_region / join / continuation
  origin: If | Ternary | SyntheticVerified
  condition_semantics

Decision
  unit: ControlUnitId
  parent_region
  selector: InstValueId
  dispatch_header / join / continuation: ControlPointId
  ordered arms: SourceDecisionArm
  default_region
  source_semantics: SourceCaseSemantics

SourceDecisionArm
  ordered patterns: [SourcePattern]
  predicate: InstValueId
  region: PredicateRegionId

SourcePattern
  EqWildcard(pattern: TypedPatternOperand,
             coercion: SourceCoercion,
             predicate: InstValueId)
  Range(lower / upper: TypedPatternOperand,
        lower_comparison: SourceComparison,
        upper_comparison: SourceComparison,
        upper_inclusive,
        predicate: InstValueId)

TypedPatternOperand
  value: InstValueId
  source domain: Bit | Logic
  width / signedness
  optional exact constant (value_bits, mask_xz)

SourceCoercion
  operand/result widths, extension/truncation, signed comparison rule

SourceComparison
  operator / signedness / SourceCoercion

GatedMux
  unit: ControlUnitId
  semantic_node: NodeId
  result / condition / then_value / else_value: InstValueId
  merge_site: ControlSite
  owner: Gate(GateId) | DecisionStep(DecisionResultMergeId, source_arm)

DecisionResultMerge
  unit / decision / merge_site
  result / default_value: InstValueId
  selected_arm_values: [InstValueId]  // one per source arm
  ordered steps: [(source_arm, arm_value: InstValueId,
                   mux: optional GatedMuxId)]
```

Each control unit belongs to one expanded combinational execution instance;
flattening appends a unit with checked ID remapping rather than merging its
root into another unit. Cross-unit region, point, value, gate, decision, or mux
references are invalid. Every emitted `LogicPath`, observer root, and runtime
event root carries its `ControlUnitId` and `ExternalRootId`; membership is never
reconstructed later from a shared arena or artificial ordering between
declarations. Roles distinguish result, local/pre-lower input, guard, ordered
argument, loop runner, position input, and effect enable/action operands.

The predicate regions are SESE ownership regions; scheduling and dominance use
the explicit control-point CFG and its dominator tree. `GateHeader`, `Join`,
and `Continuation` are distinct even when a first lowering could place them at
the same machine offset. This represents sequential gates in one parent region
without pretending that every action in the parent executes at one point.
The verifier also builds post-dominance. A region's entry dominates all of its
points, its exit post-dominates all of them, every edge entering the region from
outside targets its entry, and every edge leaving it originates at its exit.
For a gate it checks the complete shape: the header has exactly the true- and
false-arm entry successors, each arm exit reaches the declared join, and the
join reaches the declared continuation with no bypass edge. Decision arms and
default obey the analogous recorded `dispatch_header -> arm/default -> join ->
continuation` shape. These are the SESE facts on which arm
exclusivity and laminar contraction rely; reachability or entry dominance alone
is not accepted.

Gated mux allocation has a separate cache keyed by versioned `(condition,
then_value, else_value, owner, merge_site)` instance IDs. It does not silently
reuse an identical raw mux owned by an unrelated source gate, nor two
same-shaped muxes whose reads/bindings came from different versions. Ordinary
pure SLT nodes retain semantic hash-consing. All IDs use checked allocation;
exhaustion is a structured producer error.

Raw ID fields are private `u32`s. Builders use `u32::try_from(length)` and
return `IdExhausted { kind, attempted_length }`; widths and counts otherwise
remain `usize` and acquire no artificial 32-bit limit. Forward references use
fallible `reserve`/`define` slots, and `finish` rejects every undefined or
doubly defined slot before exposing the artifact.

The symbolic evaluator passes an explicit `(PredicateRegionId, ControlSite)`
through statement and expression evaluation. It does not keep
an implicit mutable "current gate" stack in the arena. `GateId` identifies one
execution instance after module expansion and unrolling; an optional source
key is diagnostic only. All output muxes created by one symbolic merge share
that instance ID, while another module instance or unrolled execution gets a
different ID. A source `case` creates one `Decision`, not a binary chain that
later has to guess selector identity. One source arm retains its ordered
nonempty list of equality/wildcard or half-open/inclusive range patterns; the
arm predicate is their disjunction, and source arms retain first-match order.
Provenance keeps the language's source matching semantics and the exact
predicate instance for each pattern even when it is four-state and ineligible
for software control. Only a later formation proof may produce a canonical
`TwoStateDisjoint` or `TwoStatePriority` decision. Ternaries and source `if`
statements create `Gate` entries with the language's exact condition semantics.
Compiler-synthesized or legacy muxes without provenance remain nodes in the
same planner, pinned to dataflow-select semantics unless a separate recognizer
produces and verifies `SyntheticVerified` provenance; there is no second old
lowering path.

The provenance verifier is written before any producer uses the table. It
checks:

- every control unit has one rooted region tree and every non-root region has
  one owner, with no cross-unit references;
- control points form the recorded CFG, action order is total within each
  point, every unit is a finite single-entry/single-exit DAG, and computed
  dominance/post-dominance agrees with every SESE region;
- gate/decision children name that owner and have the declared parent;
- each gate/decision header, join, and continuation belongs directly to its
  parent region, while every arm/default entry and exit belongs directly to
  the corresponding child region;
- every gated mux's semantic node exists, its result and ordered operand
  instances reconstruct that exact mux at the recorded merge site, and its
  owner is exact;
- every source pattern's recorded predicate exactly implements its wildcard or
  range rule, an arm predicate is their nonempty disjunction, and every
  decision result merge preserves source priority/default order;
- selector, patterns, and conditions have nonzero and compatible widths;
- condition semantics agree with the source construct and any claimed
  `KnownTwoState` fact;
- all control entries are reachable from their unit entry and all instantiated
  values/actions are backward-reachable from that unit's scheduled roots or
  recorded control semantics;
- side-table roots and actual logic paths/observers/runtime events are in exact
  one-to-one correspondence by external ID, kind, role-tagged operand order,
  action, and unit, with no missing, duplicate, or extra root; and
- serialization/deserialization rebuilds caches without changing IDs.

Unit-CFG acyclicity is checked with a worklist/topological count, without a
depth or iteration cap. Source loops have already been statically expanded;
runtime `ForFold` remains one pinned action/value and never introduces a
control-point backedge.

`SLTNodeFacts` is a prerequisite artifact shared by this verifier and existing
lowering; it is not the current recursive `get_width`. An explicit worklist
first checks every node/operand ID and rejects cycles, then computes widths with
checked arithmetic and checked slices. Equality, relational, logical, wildcard
equality, and wildcard inequality produce width one; `Mux` applies the declared
arm coercion and produces their maximum width; concat uses checked addition.
Selector/condition sites additionally require nonzero width. This same fact
table becomes the sole width API so verifier and lowering cannot disagree or
panic on malformed IDs, underflow, overflow, or a deep/cyclic graph.
Two-state facts for input leaves come from an explicit verified
`InputSemanticFacts` context built from the declaration/flattened variable type
table; `SLTNode::Input` alone does not encode `Bit` versus `Logic`. Derived
zero-mask facts are recomputed over the checked node DAG. A serialized boolean
or producer-supplied tag is never accepted as the proof.

Deserialization invokes one centralized `SLTNodeArena::rebuild_caches` routine.
Among ordinary, non-gated duplicate semantic nodes, the lowest `NodeId` is
canonical and all parser/flattening paths use that rule. Every
`GatedMux.semantic_node` is excluded from the ordinary cache even when its raw
`SLTNode` equals an ordinary or differently owned mux. The gated cache is
rebuilt independently from the complete versioned owner/merge-site key and
must reproduce that record's own ID. Control/value caches are checked
analogously, while serialized IDs and unit isolation remain unchanged.

During step 1 the arena field is `Option<ControlProvenance>` solely for reading
legacy serialized arenas: `None` means metadata absent, while `Some` must pass
the full verifier and `Some(empty)` is invalid. New-planner entry points call
`require_verified_control`, for which `None` is an error; there is no
verify-or-ignore API. The old single-root `map_addr` rejects metadata-present
arenas because it cannot freshen unit/owner IDs safely. Step 2 replaces it at
that boundary with an atomic unit mapper that maps all roots with one node map,
freshens every control/value ID per module instance, appends the completed unit,
then verifies it. Provenance can neither disappear nor alias between two
flattened instances.

Metadata failure is a producer error. It never causes an unverified attempt to
reconstruct a decision from the mux chain.

Decision-result verification first checks one selected value for every source
arm (including the unchanged/default value when that arm does not assign the
result). It then starts at the recorded default and folds source arms in reverse
priority order. An arm whose value equals the current tail may be an explicit
verified identity step; otherwise exactly one recorded mux must have that arm
predicate/arm value as its true input and the current tail as its false input.
The final instance must equal the recorded result. Every decision-owned mux
occurs in exactly one result merge. This proves both the per-arm edge values and
the multi-output priority/default chains before profitability decides whether
the decision remains dataflow or becomes control.

## 2. Maximal control skeleton and DAG placement

Before building the maximal skeleton, `ControlEligibilityPlan` classifies every
source gate/decision with an independently checked proof or a semantic
`PinnedDataflow` result. A branch is legal only when its source condition
semantics are branch-equivalent and the condition has a `KnownTwoState` proof
(a `Bit` value or an independently verified zero mask). A `Logic` condition is
not made branchy merely by attaching a two-state tag. Four-state ternaries whose
X/Z behavior cannot be represented by branch control and nodes containing
`ForFold`, runtime events, stores, or other effects are pinned to their existing
semantics.

For a source decision, eligibility replays each recorded typed wildcard/range
predicate and coercion. It requires a two-state selector, constant normalized
patterns, and proves an exact mapping to canonical masked or signed/unsigned
range patterns. Wildcard X/Z bits may become zero care bits only when that is
the source operator's verified meaning; range bounds must have zero X/Z mask.
The lower-bound and upper-bound comparisons each retain and replay their own
width/sign coercion; they may form one canonical range only when both prove the
same normalized selector domain and interval. Signed extension and truncation
are preserved explicitly, and all ranges in one canonical decision must prove
one common comparison order.
This proof exists before placement or profitability. A source case that cannot
be canonicalized stays as its already verified ordered predicate/mux dataflow;
formation is never allowed to discover semantic ineligibility after the DP.

The first skeleton contains every source gate/decision proved eligible by that
artifact. Eligibility is separate from profitability.

Scheduling keys are not bare `NodeId`s. The source-DAG artifact uses distinct
names from the later machine-memory analysis:

```text
ValueKey = InstValueId

InstValue
  unit: ControlUnitId
  semantic_node: NodeId
  ordered_operands: [InstValueId]
  direct_memory_reads: [(WriteDomainId, version)]
  direct_environment_reads: [(BindingId, version)]
  memory_dependencies: SLTMemoryDependencyId
  environment_dependencies: SLTEnvDependencyId
  execution_safety: Total(SpeculationProof) |
                    DomainRestricted(PredicateRegionId)
```

`InstValueId` is a checked ID into a hash-consed, structurally versioned value
table. The owning unit and ordered operand instance IDs are part of identity,
so values are never shared across independently scheduled control units and
noncommutative
`old_x - current_x` and `current_x - old_x` cannot collide even though their
raw `NodeId` and transitive version sets are equal. A leaf input records, at
its ordered read action, the current version of every write domain that may
alias it. Loop/function-local bindings similarly record their exact
iteration/environment version. Thus the same semantic `NodeId` is instantiated
as different values across a relevant store or environment change without
invalidating it for unrelated changes.

`SLTMemoryDependencyId` is a checked ID into an interned immutable sorted set
of the exact `(WriteDomainId, version)` facts on which an instance transitively
depends; the set may contain two versions of one write domain. The analogous
`SLTEnvDependencyId` summarizes binding/iteration facts. These sets support
alias, placement, and move-legality proofs, but are not value identity because
they deliberately discard ordered operand association.

Purity does not imply speculatability. `ExecutionSafetyAnalysis` classifies an
instance as `Total` only with an operation-specific, independently recomputed
proof that eager execution cannot trap, fault, publish an effect, or change X/Z
semantics. Division/remainder additionally require divisor-nonzero and signed
overflow safety (or a proved total lowering); dynamic memory reads require
address/fault and version proofs. Otherwise the value is
`DomainRestricted` to the exact predicate region in which the source occurrence
executes. That region is part of `InstValue` identity. Two identical
non-speculatable expressions originating in disjoint arms therefore remain two
instances; total values canonicalize to the unit root and may share. This is
linear in recorded source occurrences and does not create combinations of path
contexts.

Alias analysis declares a finite set of write domains and the sparse
`may_write_domain(read_class)` relation. A static nonaliasing store advances
only its exact domain; every overlapping read class names that domain. A
dynamic, containing, or pointer write advances the conservative domain chosen
for it, and every read it may affect names that domain. The global domain is
therefore present in every read signature and is advanced by a completely
unknown write. The verifier checks that the relation conservatively covers
every may-alias pair; uncertainty maps to global rather than omitting a fact.

Stores, version advances, releases, runtime events, and captures are ordered
actions connected by memory/effect tokens. `SLTVersionTable` verification
independently walks those actions, recomputes counters, ordered operand-instance
hash-consing, dependency summaries, and every `ValueKey`. A planning unit is
the maximal scheduler layer segment whose action graph and version tokens are
explicitly present; no value moves across an unrepresented effect boundary.

Every direct memory/environment read also receives a path-sensitive
version-validity set. `SLTVersionTable` computes reaching version tokens at
every `ControlSite`; a site is valid for the read only when every path reaching
it has the exact recorded token for every may-write domain/binding. At a merge,
`{v0, v1}` is not proof of `v0`. Equivalently, each path's first version kill
forms a frontier, but there is no assumed single linear "next action".
ScheduleLate selects only from valid sites on the earliest-to-latest dominance
path, and the verifier independently recomputes that membership. This prevents
moving `read x@v0` after a write that creates `v1` while still allowing it in a
non-writing arm where `v0` reaches. A transitive consumer may legally execute
later only through the already materialized `InstValueId` operand; it may not
silently reissue the old load.

Placement is expressed at action boundaries, not merely by predicate region:

```text
ControlSite = (ControlPointId, slot: usize)
```

For a point with `N` ordered actions, slots `0..=N` are the positions before,
between, and after them. An action at index `i` executes between slots `i` and
`i + 1`; a CFG edge leaves the predecessor's final slot and enters successor
slot zero. Point dominance plus slot order defines site dominance. This
distinguishes a gate header, join, and continuation within one parent region
and prevents a value from moving across an effect merely because both actions
have the same region owner.

The placement algorithm follows the ScheduleEarly/ScheduleLate structure used
for sea-of-nodes global code motion:

1. Build direct def-use and user lists once from all ordered roots and
   memory/effect actions in one planning unit.
2. In topological order, compute each `InstValue`'s earliest legal `ControlSite` from
   its operands, version facts, pinned memory/effect constraints, and required
   execution domain.
3. In reverse topological order, compute its latest `ControlSite` as the LCA of
   all already placed ordinary users in the expanded site-dominance tree.
   Gamma/merge operands use the final site of their actual arm predecessor as
   the use site. A gate-owned mux contributes fixed operand-use sites: its
   condition at the gate header immediately before dispatch, and each arm only
   at its corresponding exit edge. A kept decision uses its selector/patterns
   at the recorded dispatch header and each `selected_arm_value` only on that
   source arm's exit edge. A contracted decision instead uses the recorded arm
   predicates/values in its ordered mux steps at the merge site. These two
   state-specific use maps are explicit inputs to the DP and final placement;
   a decision-owned mux is never treated as an unnamed binary gate.
4. Choose the latest legal site on the earliest-to-latest dominance path that
   minimizes the pressure/loop-frequency cost. Each instance is emitted once;
   values sharing a site are emitted in verified def-before-use topological
   order.

Control-site dominance uses iterative Euler numbering and an RMQ/binary-
lifting LCA index. The verifier recomputes action slots and requires each
earliest/latest/selected triple to lie on one dominance path. A value may cross
arms only through an explicit gamma/merge result; a raw cross-arm use is a
producer error. A `DomainRestricted` assignment must be contained by its
region entry/exit; it cannot be hoisted to the LCA of disjoint arms. The planner
does not manufacture path-context combinations: distinct restricted instances
already correspond to distinct recorded source occurrences. Each value and use
edge is processed a constant number of times, for
`O((values + uses) log control_sites)` time and linear storage.

This placement gives the desired shared-expression rule directly. A value used
by both arms is placed at their common parent. A value shared only inside one
arm stays in that arm. A value shared by several outputs governed by one source
gate is computed once for that gate, rather than rediscovered independently by
each output mux.

### Profitability without a placement fixed point

Gate selection and placement affect one another, but they do not form a retry
loop:

1. Compute legality envelopes once on the maximal eligible skeleton.
2. Give every `ValueKey` exactly one cost owner: the lowest gate/decision whose
   keep/contract state can change that value's execution predicate. Values
   wholly owned by a child occur only in the child summary; values shared by
   children occur once in their parent summary.
3. For both states of each laminar region, build a `RegionStateSummary`. Its
   interface contains owned-value execution predicates, fixed state-specific
   use sites, incoming/outgoing value chunks, intrinsic/control/copy work, and
   reach weights. The pressure term is the additive chunk count on the minimal
   dominator subtree connecting those fixed uses, accumulated with Euler
   interval differences. It is a site-independent lower-bound proxy, not the
   pressure of a provisional or final placement. Child summaries expose only
   boundary values and cost-per-invocation, so composing a parent neither
   recounts child values nor expands descendant contexts.
4. Run one bottom-up dynamic program. For every gate/decision compare `kept`
   against `contracted`; a kept state pays control, expected misprediction,
   merge/copy, and live-frontier costs, while a contracted state pays eager arm
   work and selects. Child summaries are stored as cost-per-invocation; the
   parent applies the reach weight for its own kept/contracted state. Runtime
   cost is primary; equal runtime chooses smaller code, an additive frontier-
   chunk pressure proxy, then fewer regions.
5. Contract every rejected region and run ScheduleEarly/ScheduleLate exactly
   once on the final tree.

All comparisons are strict expected-cost comparisons. There is no arm count,
node count, region count, CFG size, or iteration budget. Dynamic profile data
may replace static weights later without changing legality or termination.
Contraction maps a subtree only into its existing parent and may not move a
value across an undecided ancestor arm. This laminar invariant makes the child
tables composable.

A contracted state is absent, rather than merely expensive, if it would execute
any `DomainRestricted` arm instance outside its required domain. The same rule
applies to decision contraction and reverse if-conversion. Only a newly verified
`Total` proof may make that eager state legal; allocation or performance failure
never changes the classification.

The selected DP emits a `CostWitness` listing each selected
`RegionStateSummary` and each owned value/control/copy/frontier term exactly
once. An independent verifier reconstructs the selected state-specific use map
and summaries from provenance and the contracted control tree, without reading
the final placement choices. A mismatch is an analysis error; it does not
trigger another selection pass. Final placement has its own verifier and is
allowed to beat or exceed the declared lower-bound pressure proxy.
Profitability remains a target heuristic, but ownership and arithmetic are
fully checked.

Selection also emits a `GateFormationPlan` with one entry for every gate-owned
`GatedMux`. For a kept gate, verification relates the original condition and
true/false operands one-to-one to the corresponding branch edges and join
arguments, including complete `InstValueId`s and widths; swapping arms requires an
explicit, independently proved condition inversion. For a contracted gate it
proves that the original dataflow `Mux(condition, then, else)` remains and that
no residual arm branch claims to implement it. It also proves that every
original gated result is represented exactly once and that no new result is
introduced. This semantic relation is checked against the emitted SIR; a
placement that merely satisfies dominance and types is insufficient.

`DecisionFormationPlan` covers every decision-owned result merge. In a kept
decision, each canonical pattern entry must map back to its source arm, every
source arm/default edge must pass that merge's recorded
`selected_arm_value`/`default_value`, and the join result must replace the
recorded final result exactly once. In a contracted decision, no canonical
terminator may claim it; the verified ordered predicate/mux DAG and its final
result remain. Thus both profitability states have an explicit condition/value
use map and an arm-by-arm semantic output proof.
The two input verifiers jointly prove that their owner sets are disjoint and
their union contains every `GatedMux` exactly once.

The independent `PlacementPlan` verifier recomputes def-use edges and proves
that every pure `InstValue` is assigned exactly once, operands dominate their users,
each assignment site lies between its earliest and latest legal sites, the
within-site order is def-before-use, gated arm nodes cannot execute on another
arm, every restricted value stays in its required execution domain, pinned
effects did not move, and every root receives a dominating value of the
declared width and memory/environment version.

## 3. Canonical DecisionRegion

A selected multiway source decision is retained in canonical SIR instead of
immediately becoming a binary diamond chain. Its conceptual terminator is:

```text
Decision(selector,
         ordered [(pattern, target, edge_args, probability, source_arm)],
         default_target + default_edge_args,
         semantics = TwoStateDisjoint | TwoStatePriority,
         range_order = Unsigned | Signed)

DecisionPattern = Masked(value, care_mask) |
                  Range(lower, upper, upper_inclusive)
```

A masked pattern matches when
`(selector & care_mask) == (value & care_mask)`; `care_mask` is all ones for an
exact equality. A range matches `lower <= selector < upper`, or `<= upper` when
inclusive, in the decision's one declared signed or unsigned order. All
range patterns in one decision use that same order; eligibility pins a source
case with mixed range-comparison orders to dataflow. All operands are
normalized selector-width bit vectors using the eligibility witness's exact
source coercion.

`DecisionFormationPlan` has two producer forms. A source `case` supplies its
selector and ordered source arms directly; an arm with several patterns is
expanded to ordered canonical entries retaining one `source_arm` identity and
one target/argument tuple. A maximal nested `if`/Gate chain may be combined only
after the formation verifier proves that every comparison uses
the same selector (modulo width-preserving identity/casts), that no effect lies
between tests, and that default/priority order is unchanged. This is the point
where separate source gates become one decision; target lowering never guesses
the relationship from machine blocks.

Accumulator-guarded priority chains have an additional canonical proof. The
accepted form is `acc = mux(guard_i && acc == default, value_i, acc)` with one
initial default, no intervening definition, and no observation of an
intermediate accumulator. The verifier checks the complete def-use chain and
proves by induction that it is an ordered first-match decision. It must also
prove at least one of: every selected `value_i` differs from the sentinel,
the guards are mutually exclusive, or a separate monotone matched bit (rather
than the result value) prevents later updates. Otherwise an earlier arm that
returns the sentinel can be overwritten and the rewrite is illegal. A partial
match, extra accumulator use, or different sentinel remains ordinary gates.

The SIR verifier checks the ordinary CFG/SSA edge contract plus:

- selector type and width, normalized key/mask widths, and zero bits outside
  the selector width, plus normalized and nonempty range bounds;
- exact-key uniqueness by sorting for `TwoStateDisjoint`, in
  `O(cases log cases)`; duplicate exact keys remain legal, ordered, and
  generally redundant in `TwoStatePriority`;
- every `TwoStateDisjoint` pattern pair is nonoverlapping: mask/mask uses
  `((value_i ^ value_j) & care_i & care_j) != 0`, range/range uses interval
  intersection, and mask/range uses the exact bit-DP described below;
- recorded terminator order and first-match semantics for
  `TwoStatePriority` arms (overlap is legal); source-order correspondence is
  separately owned by `DecisionFormationPlan`;
- one default edge, total edge argument arity/types, and target existence;
- an explicitly two-state selector value, until four-state X/Z branch behavior
  is specified. An inherent `Bit` value qualifies directly. Otherwise the
  producer must insert a typed `KnownTwoState` conversion whose zero-mask fact
  is recomputed by the standalone SIR verifier from the value's definitions;
  a semantics tag or unverified annotation alone never qualifies.

Before SIR construction, a separate `DecisionFormationPlan` verifier relates
every arm back to the same provenance selector and proves the source pattern,
priority, and default mapping. The standalone SIR verifier does not trust or
need access to the discarded SLT arena; it rechecks the complete canonical
CFG, type, key, mask, and edge contract.

The baseline disjoint verifier is deliberately `O(cases^2 * selector_chunks)`;
it is finite, exact, and is not a case-count cap. Range/range overlap is an
ordinary interval-intersection check after every selector/pattern in a signed
decision is order-normalized by flipping the sign bit. Mask/range overlap uses
a linear bit-DP in that same common normalized domain
that proves whether any selector in the interval satisfies the fixed mask; it
does not enumerate selector values. Overlapping wildcard/range
decisions remain ordered priority decisions and need no disjointness proof. A
target forms the precedence graph containing `i -> j` for every originally
ordered pair `i < j` whose patterns overlap. Its reordered cases must be a
topological ordering of that graph. Therefore disjoint cases may cross, but
the relative order of every overlapping pair is preserved; proving only that
the moved subset is internally disjoint is insufficient.

Every logical case/default edge has a distinct `DecisionEdgeId`. Before phi
construction it is materialized as a case-specific trampoline, even when two
cases name the same target, because their edge arguments may differ.
Every case also retains an immutable `DecisionCaseId`, `SourceArmId`, and dense
`PriorityRank` established by the formation output relation. The standalone
verifier requires every overlapping pair to appear in increasing rank order.

Every backend must understand the canonical terminator. The initial semantic
implementation may legalize it to an ordered branch tree, but this is a normal
target lowering selected before code generation, not an error fallback. Native
code keeps exact/disjoint decisions long enough to select a table. Cranelift
and Wasm use those trampolines/dispatch blocks because their table primitives
do not directly model arbitrary per-case SIR block arguments or structured
labels.

### Target strategy selection

`DecisionLoweringPlan` is an explicit, verified artifact. Candidate strategies
are:

- constant-result value lookup table for an exact-key disjoint decision whose
  cases and default have no effects and pass only one constant result to one
  merge;
- bounds check plus dense jump table for exact, disjoint keys;
- bit tests for small result/target sets;
- probability-weighted balanced comparison tree for sparse exact keys and
  proved nonoverlapping ranges;
- ordered early-exit chain for priority/wildcard/range arms; and
- a hybrid whose cost-selected tail is reverse-if-converted to `cmov`/select
  only when every tail arm is pure/eager-safe and first-match order is retained.

The target enumerates applicable strategies and minimizes:

```text
expected dynamic comparisons, branches, misses, loads, and edge copies
then code bytes + read-only table bytes
then pressure/live-frontier cost
```

Planning consumes the canonical decision only after all target-independent SIR
optimization has completed and the decision has been re-verified. Native ISel
then preserves it as `MDecision`; strategy legalization consumes the plan and
immediately checks the MIR/table output relation. No unrelated optimizer runs
between a witness and that output and no later pass may rewrite the decision
without producing a new verified plan.

Applicability is proved from key range, target support, and semantics. Density
or case count may appear in the cost equation, but never as a correctness or
termination cutoff. Sparse-tree splitting follows probability-weighted
near-optimal search-tree construction; jump-table and bit-test clusters follow
the same separation used by LLVM/GCC switch lowering.

A value lookup table covers the inclusive `[minimum_key, maximum_key]` span.
The generated code performs a full-selector-width unsigned bounds check before
any subtraction, truncation, or scaled indexing; out-of-range values select the
default. Every hole in the span is initialized to the default result. The plan
verifier checks arithmetic overflow, entry count, every key-to-index mapping,
all hole/default entries, result width, and that the eventual index conversion
cannot discard selector bits before the bounds proof.

The lowering plan contains a concrete `LoweredDecisionWitness`, not just a
strategy name and cost. Its comparison/range/bit-test/table nodes refer to the
original full-width selector, and every leaf names the original
`DecisionEdgeId` and exact edge arguments. Strategy-specific verification
accepts only a typed `DecisionTestGraph`: pattern-test/compare nodes, verified
bit-set or table dispatch nodes, reverse-fold select-tail nodes, and original
edge/default leaves. It does not attempt equivalence on an arbitrary CFG.

For a disjoint strategy, interval/key/bit/table construction rules prove every
pattern's entire match set reaches the same leaf and the complement reaches
default. For priority, the verifier checks an exact permutation of canonical
patterns plus the overlap precedence graph, then structurally replays an
ordered `Test(pattern, match_leaf, next)` chain or a reverse select fold with an
explicit monotone matched bit. Induction on that representation proves arm
`i` receives exactly `Match_i \ union(Match_j, j < i)` (possibly empty for a
shadowed duplicate) and default receives the complement; no exponential set
representation is built. An output
verifier then matches that witness arm-by-arm to the actual lowered SIR/MIR
CFG, trampolines, and table artifact. Ordinary CFG validity cannot substitute
for this cross-phase semantic proof; a locally valid graph with two case edges
swapped must fail.

Native ISel exposes a multi-successor `MDecision` terminator to CFG, liveness,
regalloc, and layout verification. Strategy legalization expands trees/chains
before allocation; a jump-table `MDecision` remains explicit through layout so
all targets stay visible. Jump tables use 64-bit absolute target entries in a
read-only data artifact plus relocation records patched after executable memory
is allocated. This avoids an optimization-dependent relative-range failure.
The baseline code obtains the separately mapped table base with a 64-bit
absolute `movabs` relocation patched after both mappings exist, so it has no
implicit +/-2 GiB code-to-data assumption. A later RIP-relative form is legal
only when `DataLayoutPlan` and the post-allocation emitter verifier prove the
signed displacement range. The emitter verifies entry count, every trampoline
label, table-base and entry relocation site/width, and default behavior before
encoding.

Before adding the SIR terminator, successor enumeration, edge identities and
arguments, register uses, renumbering, dominance traversal, serialization, and
display are centralized in common `SIRTerminator` APIs. Optimizer and backend
passes consume those APIs; adding a case edge cannot be silently omitted by an
old exhaustive match.

Pattern/order/edge-identity fields are private and immutable through ordinary
SIR optimization; case/default targets and the edge-argument list, order,
roles, and types are likewise opaque. Register/block renumbering may update
only typed references through the common API. The function retains the
formation-verified decision-origin table, and every post-pass verification
compares selector identity, case/source-arm IDs, patterns, priority ranks,
targets, edge identities, and edge-argument correspondence against it. An
optimizer that genuinely rewrites a decision must
instead emit a `DecisionRewritePlan` relating the old and new test graph and
pass its semantic output verifier before the origin table is replaced. Swapping
overlapping priority arms and swapping their ranks together is therefore not a
way to evade verification.

## 4. Predicate-aware vectorization

SLP/store combining runs after the final control tree exists. A pack may contain
only operations with the same execution predicate and compatible memory
dependence. It may combine adjacent scalar loads/stores or wide copies with
XMM/target vector operations, but it cannot pull an unselected arm back into
eager execution. Compare+branch, fixed-register sequences, release/event
publication, `MemCopy`, and an established SLP pack are indivisible bundles for
later pressure cuts.

The initial native pack is deliberately a `VectorMemPack`: an indivisible
memory-transfer bundle whose XMM scratch registers are fixed internal
temporaries with explicit uses/clobbers. No vector virtual value, phi, bundle
input, or bundle result may be live outside it; externally visible values
remain memory or ordinary GPR values. Its verifier checks same predicate,
alias/order, alignment/width legality, complete byte coverage, fixed-scratch
availability, and zero XMM live-in/live-out. The pressure model charges the
bundle's class-specific scratch occupancy, while the existing `K = 14`
coloring contract remains specifically the GPR class. Arithmetic SLP with live
vector values is not enabled until class-specific liveness, homes, Perm
matching, pressure cuts, and coloring have their own verifier contract; it is
not smuggled through the GPR allocator.

For the initial implementation, "same execution predicate" means the scalar
operations are ordinary unpredicated instructions in the same verified MIR
basic block after decision legalization, with no branch/effect boundary inside
their dependence window. `MIRBlockId` is the predicate witness and the output
verifier rechecks membership/order. Cross-block packs are inapplicable until a
persistent SIR-to-MIR predicate identity and equivalence verifier are designed;
the provenance predicate is not guessed from layout.

`SLPPlan` is a semantic witness. For each pack it records every replaced scalar
operation, its execution predicate and order, source/destination byte range,
the exact scalar-bit to vector-lane/byte mapping, target endianness, alignment
fact, vector width, and fixed scratch/clobber set. Verification proves the pack
touches exactly the union of the scalar bytes with no gap or extra access,
preserves load/store order for every may-alias pair, has identical fault/
addressability bounds, and reconstructs each scalar result or store byte.
An output verifier matches the witness to the emitted vector bundle and proves
all replaced scalar operations disappeared exactly once. A same-predicate or
same-width heuristic without this lane/byte proof is not an accepted pack.

## 5. PressureRegion planning

A `PressureRegion` is a full register cut inside one native function. It is not
a separate function and does not impose a call ABI. The function keeps one
prologue/epilogue; regions connect with direct jumps/fallthrough. R15 (the
simulation-state base) and RSP are the only implicit machine state across a
full cut.

Every cross-region logical value has exactly one complete recipe:

```text
CrossValueMaterialization =
    ConstantRemat(value_bits, mask_bits, width)
  | StateReload(StateReloadRecipe, lazy reconstruction sites)
  | BoundaryHome(home,
                 edge-sensitive source stores,
                 lazy reload/reconstruction sites)
```

The initial rematerialization form is only an exact value/mask constant; an
expression rematerializer requires a future independently checked semantic
recipe. `BoundaryHome` is one paired materialization kind, not separate
"store" and "reload" choices. Its stores cover every incoming cut edge with
that edge's logical source, while reloads remain lazy near successor uses.

Reloading every value at region entry is forbidden because it recreates the
pressure. Existing pruned-IDF reconstruction places fresh representatives and
merge phis. For a phi crossing a cut, every predecessor stores its
corresponding logical source to the same home; the all-path home verifier proves
the successor reload.

Candidates come only from semantic frontiers: decision entry/merge edges,
completed top-level store roots, memory-effect component boundaries, and
verified loop/SCC entry or exit edges. A legal instruction frontier is first
split into real MIR blocks, before CFG normalization, CSSA, next-use, loop, or
spill analysis. `PressureRegion` therefore partitions whole blocks and cuts
only real edges; it never invalidates an analysis by splitting a block after
that analysis. Compare/branch, machine-constraint, release/event, `MemCopy`,
and SLP bundles cannot be split. Fixed instruction counts and maximum region
counts are not candidates or guards.

`MIRMemoryVersionAnalysis` is an explicit post-scheduling artifact, separate
from `SLTVersionTable`. It recomputes reaching versions from actual MIR
memory/effect order using the same `WriteDomainId` and conservative
read-class-to-domain relation. If lowering must refine domains, it records an
explicit conservative mapping back to every SLT domain; equality of unrelated
numeric IDs is never assumed. Lowering records checked memory-origin links
where they exist, and a cross-artifact verifier checks the same state object,
read class, and complete mapped write-domain set. Absence of such a link only
disables state reload for that value.

The unchanged-version fact alone is not a reload recipe. Every proposed state
reload carries a `StateReloadRecipe` identifying the state object/address,
value and mask lanes where applicable, byte/bit slice, load widths, endianness,
concatenation order, and zero/sign/no-extension operations needed to reproduce
the exact logical value. Its verifier symbolically checks the recipe against
the value's defining origin and proves that every referenced byte has the same
reaching MIR version at the reload. Only that semantic equality proof permits
the versioned-state option.

PressureRegion selection precedes, but does not iterate with, the one
Braun--Hack spill plan. A composable `PressureCostSummary` uses an additive
target proxy:

```text
cost = sum(reach_weight * excess_live_chunk_integral * spill_unit_cost)
     + sum(cut_edge_weight * exact_boundary_store_reload_work)
     + transfer/copy work
```

Every live-range contribution is owned by its deepest laminar region and
aggregated with Euler interval differences, so child summaries compose without
running MIN for each alternative. This is a checked profitability model, not a
claim that the proxy equals the eventual optimal spill count. Laminar decision
regions use one bottom-up `fuse` versus `full cut` DP. General CFG is first SCC
condensed; irreducible and reducible loop SCCs are atomic in the initial design,
so there is no undefined internal cut-set. On the resulting acyclic graph,
dominance/post-dominance constructs a canonical laminar
`PressureRegionTree` of verified SESE regions. A semantic frontier is a legal
cut candidate only when it is the complete incoming and outgoing edge boundary
of one tree node; crossing candidates and rigid residual subgraphs remain
atomic. Every
block belongs to exactly one leaf/residual owner, so deepest ownership and
Euler aggregation are defined. The same bottom-up DP either fuses a child or
cuts both sides of its complete boundary, charging materialization in both
directions; it is never applied directly to a general DAG. After selection,
remove the union of selected boundary edges and compute the maximal weakly
connected components of the remaining CFG. Those components, not a tree node
with a hole, are the final pressure-region partition; parent prelude, child,
and parent continuation may therefore be three regions. The verifier proves
every original edge is either internal to exactly one component or is an
explicit full-cut edge, and recomputes all cross-value recipes on that final
partition. The acyclic graph uses finite
integer `ReachWeight`s from profile counts or normalized static edge weights;
an opaque residual/loop's internal work is identical in both boundary
alternatives.
After selection, forced cut facts and ordinary MIN decisions are combined in
one final `SpillPlan`. It is materialized once; coloring failure never requests
another cut.

The frame contains a boundary-home area and one reusable regional spill arena.
Boundary homes initially receive unique identities with size/alignment
requirements; there is no unproved memory-live-range slot coloring. Full cuts
prove that region-local homes do not survive into another region. The final
`SpillPlan`, after ordinary homes are known, owns concrete offsets and the
complete frame layout; it allocates one arena sized and aligned to the maximum
regional requirement rather than the sum of regional frames.

Input `CutPlan` verification proves region partitioning, legal edges, exact
edge-sensitive planned cross-value sets, one valid materialization kind per
value, MIR memory-version/reload-recipe facts, and boundary-home identity plus
size/alignment. It does not assign final offsets or claim that
pre-materialization liveness is already empty. The final `SpillPlan` verifier
proves concrete boundary and regional-home offsets, arena maximum reuse, frame
nonoverlap/alignment/bounds, and ordinary home ownership. A separate
post-reconstruction `CutResult` verifier proves all-path stores, zero ordinary
register liveness across each full cut, reload dominance, phi meaning, and the
existing pressure/home/Perm contracts against that final layout.

## 6. Final phi-congruence affinity

Method-I CSSA before home formation remains unchanged. After reconstruction
and Perm materialization, the final phi graph is rebuilt with a DSU and its
components are classified by an independent interference check. A component
proved conventional may use component-wide preferences. A non-conventional
component is still valid MIR, but receives only direct-pair and ordinary
weighted affinities. It is not silently treated as CSSA and is not a reason to
renormalize after Perm.

Components are not contracted to one mandatory color. Coloring uses a
component-wide soft preference:

- one consistent required color, or a nonempty intersection of future allowed
  masks, may seed the preferred color;
- conflicting required colors or an empty allowed-mask intersection produce
  no class seed (never an error by themselves); members then use only feasible
  local/weighted affinities;
- the first feasible member may establish a preference for later siblings;
- two-address and weighted phi affinities contribute preferences;
- required colors, forbidden/clobbered colors, the current active set, and Perm
  perfect matching always take precedence; and
- if the common color is unavailable, that member is colored locally and the
  residual move remains for verified SSA destruction.

This propagates color intent between sibling decision arms and toward future
RCX/RAX/RDX constraints without making coloring failure trigger spilling. The
classification verifier reuses the streaming edge-sensitive liveness analysis
and accounts for `O(instructions + uses + phi rows + sparse live facts)` work;
DSU/selection then costs
`O((vregs + phi rows) alpha(vregs) + K * vregs)` with `K = 14`.

## 7. Copy-aware block layout

Block order becomes a separate post-coloring artifact. SSA destruction first
materializes a typed `EmissionFragment` graph containing real blocks and
case/edge-copy code fragments plus referenced read-only table fragments; copy
stubs are no longer hidden local labels for layout purposes. Code and read-only
data receive separate mappings. Trace formation orders only executable
fragments and uses edge probability and actual copy work so the likely edge,
merge, and copy fragment can fall through together. It does not change CFG
semantics. `BlockLayoutPlan` verification proves that every executable fragment
occurs exactly once, entry is first, every fallthrough targets the next
executable CFG successor, branch targets are unchanged, copy code remains on
its selected edge, and no executable edge reaches a data fragment.
`DataLayoutPlan` separately proves every referenced table occurs once with
valid alignment/relocations; all code and table labels must resolve.

## 8. Termination and complexity contract

All trees and graphs use explicit worklists; deeply nested source control and
phi webs must not recurse on the host stack.

- node/provenance/control verification: linear in SLT nodes/edges, control
  points, gates, decision arms/patterns, and merge steps, plus documented
  dominator work;
- def-use plus placement: `O((values + uses) log control_sites)`;
- gate and laminar pressure DPs: linear in their region trees;
- exact-key clustering: `O(cases log cases)`; disjoint pattern verification:
  `O(cases^2 * selector_chunks)`;
- decision-test witness replay: linear in lowering graph nodes plus the same
  pairwise overlap relation;
- CFG/SCC/SESE analysis: `O(blocks + edges)` (or documented near-linear
  dominator cost);
- SLT and MIR version analyses: proportional to their memory actions, alias
  edges, dependency sets, and sparse reaching-version facts;
- pressure summaries: linear in owned live-range events plus the region tree;
- cut materialization: proportional to actual cross-region values;
- final congruence classification: linear in MIR/phi edges plus sparse liveness,
  followed by inverse-Ackermann DSU; and
- coloring/layout/emission: linear in MIR, CFG edges, and emitted table/copy
  entries for fixed `K`.

There is no iterative branchification, allocation retry, packed 24-bit ID,
input-dependent traversal limit, CFG cap, or legacy correctness fallback.

## 9. Verifier-first implementation sequence

1. Add checked `ControlUnit`/control-point/site types, iterative
   `SLTNodeFacts`, structurally versioned `InstValueId`/`SLTVersionTable`, and
   the complete provenance foundation verifiers plus serialization/cache tests.
   These artifacts are verified in dependency order; do not change lowering
   output yet.
2. Make symbolic evaluation produce those artifacts for every declaration and
   flatten-remapped instance. In diagnostic mode, build and verify
   `ExecutionSafetyAnalysis`, `ControlEligibilityPlan`, the maximal
   `ControlSkeleton`, state-specific use
   maps and legality envelopes, `RegionStateSummary`, the one bottom-up DP and
   `CostWitness`, contraction, the one final `PlacementPlan`, and
   `GateFormationPlan`/`DecisionFormationPlan`; report the 3,227 currently
   rejected cases. This step remains diagnostic and does not switch lowering,
   because canonical Decision SIR is not available yet.
3. Centralize SIR terminator use/edge/renumber APIs, then add canonical
   `Decision` SIR plus malformed-input verifier tests. Teach all backends the
   semantics through explicit trampolines/legal lowering before any native
   jump-table optimization. Re-run the complete step-2 pipeline, formation
   output relations, optimizer decision-origin checks, and backend semantic
   tests; only then make it the sole source-DAG lowering path.
4. Add explicit multi-successor native `MDecision` verification and verify
   `DecisionLoweringPlan` plus its `LoweredDecisionWitness` output relation,
   starting with
   sparse balanced trees and dense jump tables; accept each with semantic and
   same-build runtime tests.
5. Add same-block `VectorMemPack` through verified `SLPPlan`. Its output then
   flows through newly rebuilt frontier splitting, scheduling, liveness, and
   every later MIR analysis; no pre-SLP fact is reused.
6. Add verified semantic-frontier block splitting,
   `MIRMemoryVersionAnalysis` plus `StateReloadRecipe`, input `CutPlan`, final
   `SpillPlan` frame-layout verification, and output `CutResult` verification;
   then constrain the single spill plan with selected PressureRegions.
7. After reconstruction, add final phi-congruence classification and
   component-wide soft affinity only for components proved conventional. Then
   add typed code/data fragments and copy/probability-aware
   `BlockLayoutPlan`/`DataLayoutPlan`, only after their input and
   output-relation verifiers exist.

Each step lands as a valid phase boundary. Existing binary lowering remains the
current implementation until step 3 replaces it after the complete verified
pipeline is available; it is never selected because a new plan failed
verification. The final acceptance gate is a successful
same-condition full Heliodor run compared with `veryl-cc`, not compile-only
status, projected time, IR size, or a partial timing window.

## References and implementation comparisons

- Cliff Click, [*Global Code Motion / Global Value
  Numbering*](https://doi.org/10.1145/223428.207154), PLDI 1995: the
  ScheduleEarly/ScheduleLate placement model.
- Jens Knoop, Oliver Rüthing, and Bernhard Steffen, [*Lazy Code
  Motion*](https://doi.org/10.1145/143103.143136), PLDI 1992: safe/economical
  placement without unnecessary register pressure.
- [LLVM `SwitchLoweringUtils`](https://www.llvm.org/doxygen/SwitchLoweringUtils_8h_source.html):
  jump-table, bit-test, and probability-weighted search-tree clustering.
- [GCC tree switch conversion](https://gnu.googlesource.com/gcc/+/refs/heads/master/gcc/tree-switch-conversion.h):
  simple-case, jump-table, and bit-test clusters.

These references supply algorithms and comparisons, not Celox's correctness
contract. The contracts above are enforced by Celox verifiers before a phase's
artifact is consumed.
