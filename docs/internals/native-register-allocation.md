# Native register allocation

> **Status:** the Braun--Hack W/S pipeline below is the current production
> allocator.  The interval-based replacement described in the next section is
> implemented through explicit home expansion, joint original/synthetic
> allocation, pressure-driven live-region splitting, and atomic strict-SSA plus
> out-of-SSA lowering.  The explicit `interval` implementation publishes that
> closed result for differential execution, while the default still publishes
> the established allocator's result. Fixed-register/clobber constraints,
> weighted coalescing, and exact stack-home slot coloring are integrated in the
> replacement; qualification and the default switch remain. A replacement is
> accepted only by correctness tests and the executable Heliodor gate;
> speculative phase designs are not normative.

The native backend treats register allocation as a verified sequence of IR
transformations.  It is not permitted to recover from an invalid MIR graph,
allocation failure, or excessive compile time by truncating work, limiting CFG
growth, or panicking.  Large functions use ordinary `u32`/`usize` indices;
packed indices with a 24-bit payload are deliberately excluded.

## Why the allocator is being replaced

The original unified allocator combines liveness, eviction, spill insertion,
physical assignment, and phi-edge repair in one forward walk.  A decision can
therefore change the instruction stream after the analysis on which the
decision was based.  A virtual register can also have an edge-local location
which is not represented by its function-wide assignment.  These properties
made correctness depend on incidental block order and produced excessive
stack traffic on large branchified functions.

The replacement follows the structure described by Braun and Hack for SSA
spill placement and SSA register allocation:

- [Register Spilling and Live-Range Splitting for SSA-Form Programs](https://pp.ipd.kit.edu/publication.php?id=braun09cc)
- [Register Allocation for Programs in SSA Form](https://compilers.cs.uni-saarland.de/projects/ssara/)
- [Revisiting Out-of-SSA Translation for Correctness, Code Quality, and Efficiency](https://inria.hal.science/inria-00349925)

Go's production allocator is also used as an implementation reference for
machine constraints and edge shuffles, not as a source of compact identifiers:

- [Go compiler register allocator](https://go.googlesource.com/go.git/+/8b25a00e6d889c8a919922f747791478c8bdfe6f/src/cmd/compile/internal/ssa/regalloc.go)

`regalloc2` is not a dependency or design target.  Its large-function behavior
and compact internal index constraints do not meet Celox's requirements.

## Interval replacement: one allocation result, one rewrite

The replacement must solve live-range splitting, register assignment, and
home placement as one allocation problem.  It must not reproduce the current
pipeline by choosing stack residency first, substituting MemorySSA reloads
later, and coloring a graph which can no longer request a different split.

Steps 27a through 27c of the throughput plan already provide useful pieces:

- stable instruction-use, definition, block-boundary, and phi-edge slots;
- exact CFG-sparse live segments and an independently recomputed liveness
  verifier;
- versioned state/rematerialization recipe DAGs at every exact use;
- physical-register interval unions, allocation-owned recoloring, eviction,
  and a first dominance-aware region splitter.

Those pieces were prerequisites, not a complete allocator. Before Steps
27d1--27d6, the diagnostic plan had three structural defects which had to be
removed before it could rewrite MIR:

1. A split currently finalizes at most one register child and sends every
   remaining use directly to a home.  It cannot discover two disjoint hot
   register regions of one machine value, and a home child cannot re-enter the
   allocation queue.
2. `stack_home_created` is only a Boolean cost/accounting fact.  It does not
   name the definition or edge which stores the value, and therefore cannot
   prove that a stack value reaches every selected reload.  Adding a store
   later in reconstruction would repeat the old error of changing liveness
   after allocation.
3. A home child had an empty live range and was treated as final.
   Real code still needs a register for the original definition-to-store,
   every reload-to-use, and every intermediate result in a state/remat recipe.
   Those synthetic machine values can interfere with already assigned roots.
   Inventing scratch registers in the rewriter would have made the physical
   allocation incomplete.

The retained 27d pipeline now removes all three defects: stack definitions are
explicit, every executable transition re-enters joint allocation, one root may
own multiple register regions, and the closed result lowers atomically into a
private MIR function. Target constraints, coalescing, and final frame coloring
are part of that closed diagnostic result. Production still uses the interim
allocator until differential execution and the Linux acceptance gate pass.

The production boundary is therefore not `AllocationPlan`.  That type remains
solver-internal and may contain queue stages, rejected parents, cached costs,
and interval-matrix identities.  Home selection first extends an off-to-the-
side allocation IR with explicit synthetic instructions and machine values.
Those values go through the same liveness and allocation queue as original
MIR values.  The solver may finish only when it can produce a closed
`AllocationResult` with machine-semantic decisions for both original and
synthetic values:

```text
AllocationResult
  allocation_ir: original plus synthetic machine definitions/uses
  roots: [AllocatedRoot]
  register_regions: [RegisterRegion]
  stack_homes: [StackHome]

AllocatedRoot
  original VReg and definition
  exact UseSite -> Location mapping

RegisterRegion
  physical register
  exact sparse live segments
  entry definition:
    original MIR definition
    or explicit Home -> Register transition at a point/edge

StackHome
  logical root identity and provisional frame class
  explicit Register/Recipe -> Stack definitions at points/edges
    or an out-of-SSA phi destination defined on every incoming edge
  exact Stack -> Register reload demands

Location
  RegisterRegion
  StackHome reload
  exact versioned state/rematerialization Recipe
  phi-edge Stack or Immediate source
```

Every original instruction use and phi-edge use has exactly one location.  A
register region has exactly one SSA definition and contains only uses dominated
by that definition.  A stack reload has an explicit reaching stack definition
on every path.  Recipe transitions retain their exact MemorySSA versions.
There is no implicit store, reload, edge copy, or register assignment left for
the rewriter to invent.

The allocation IR is not partially valid MIR and is never visible to another
optimizer.  It records insert-before/insert-after/edge anchors against the
immutable input function, synthetic opcode DAGs, and exact def/use identities.
Its liveness index accepts original and synthetic machine values uniformly.
Successful completion lowers it into a new strict-SSA `MFunction` atomically;
failure leaves the input untouched.

Allocation-session coordinates are block-local, and synthetic instruction and
machine-value identities are monotonic. Splitting may make an identity dead but
never renumbers a surviving one; its liveness row becomes empty. This is what
allows a later session to update affected sparse ranges without invalidating
unrelated blocks or allocation units.

Physical interval-union bundle identity is the same stable VReg identity, not
the compact row of currently active values. The joint allocator retains its
matrix and assignments across split transactions; unchanged ranges stay
resident while dead, rewritten, displaced, and new values re-enter the work
queue.

Allocation-session liveness uses immutable original-instruction identities,
monotonic synthetic identities, and stable physical slots. Allocation IR emits
an exact def/use journal while a split round is being mutated. Synthetic rows
receive identities immediately, but each touched block publishes its dense
instruction snapshot with one ordered merge at the round boundary. Dense
positions are therefore lowering metadata rather than mutation identities.

The liveness session applies each block fact row and each VReg use row with one
ordered merge. The fact index and `LiveInterval` share the resulting immutable
use row, and sparse range reconstruction reuses one epoch-marked CFG workspace.
Unchanged stable ranges retain their matrix token and assignment. Ordinary
dense-slot MIR still keeps value-by-block membership because inserting a dense
position genuinely relabels crossing ranges. Copy/phi affinity weights and
fixed-register reservations are likewise session-owned sparse indexes with
explicit revision publication.

Debug builds, or optimized builds with `CELOX_REGALLOC_VERIFY`, compare every
exact transaction with a changed-block rescan. Optimized production performs
the independent complete liveness, machine-fact, allocation, lowering, and
physical-matrix proofs once at the atomic publication boundary. On the retained
Linux workload this transaction model restored non-LTO compilation to about
56 seconds and completed at `cy=9ae070 x3=aa pass=1`. Compile latency is no
longer the blocker exposed by the earlier differential updater; integrated SSA
fragment allocation and spill placement remain open, and peak memory has not
yet been requalified.

### Replacement allocator architecture

The production interval allocator is being replaced with the architecture of
LLVM's greedy register allocator.  This is a structural constraint, not a list
of heuristics to graft onto the existing joint solver.  The reference
implementation is LLVM's
[`RegAllocBase`](https://github.com/llvm/llvm-project/blob/main/llvm/lib/CodeGen/RegAllocBase.cpp),
[`RAGreedy`](https://github.com/llvm/llvm-project/blob/main/llvm/lib/CodeGen/RegAllocGreedy.cpp),
[`LiveRangeEdit`](https://github.com/llvm/llvm-project/blob/main/llvm/include/llvm/CodeGen/LiveRangeEdit.h),
and
[`SplitKit`](https://github.com/llvm/llvm-project/blob/main/llvm/lib/CodeGen/SplitKit.cpp).

The ownership boundaries are normative:

1. `LiveIntervals` owns every current machine live range.  A range is the unit
   placed in the work queue; a semantic RTL root and a preselected memory home
   are not allocation units.
2. `LiveRegMatrix` owns current physical assignments and fixed target
   interference.  An assignment may be removed.  Occupancy from an earlier
   queue decision is never treated as an immutable global solution.
3. The base driver dequeues one unassigned interval and calls
   `select_or_split`.  That operation returns either one physical register or
   a set of edited/new intervals.  The driver alone assigns the returned
   register and requeues every live child.
4. Each interval carries the stages `New`, `Assign`, `Split`, `Split2`,
   `Spill`, and `Done`.  The first failed assignment is deferred at `Split`, so
   splitting observes the matrix after the primary assignment queue has
   settled.  `Split2` requires measurable range reduction.  Only spiller
   products enter `Done`.
5. Eviction uses a monotonically increasing cascade number.  Every victim is
   unassigned and requeued at its existing stage.  A victim is not converted
   to a terminal no-eviction leaf; equal or newer cascades alone prevent an
   eviction cycle.
6. `SplitAnalysis` reads block/use topology and physical interference.
   `SplitEditor` performs one private allocation-IR edit, creates real child
   VRegs and exact child intervals, and returns all children.  It does not
   choose their final colors and does not finalize the complement to memory.
7. The spiller runs only after assignment and both split stages fail.  It owns
   stack placement and asks Celox's MemorySSA/rematerialization analysis for
   the cheapest valid value reconstruction at each insertion point.  Those
   HDL-specific recipes are spiller inputs; they are not physical-register
   allocation states.
8. Allocation IR remains private until every live interval has either a
   physical assignment or a proved memory/rematerialization location.  MIR is
   rewritten once, followed by an independent physical-liveness verifier.

The following are therefore rejected designs: one global home decision per
RTL root, final `Home` children created by splitting, immutable symbolic child
colors, repeated whole-problem publication between split rounds, and a custom
solver that simultaneously chooses topology, colors, and memory recipes.

Celox differs from LLVM only where the source language provides additional
facts.  CFG-sparse ranges may share a physical register across mutually
exclusive RTL control-flow paths, and MemorySSA may prove a state load or pure
expression cheaper than a stack reload.  Both fit behind the conventional
matrix and spiller interfaces; neither changes the worklist protocol.

LLVM's greedy allocator edits a post-SSA machine function and represents one
virtual register with multiple value numbers.  Celox deliberately keeps its
private allocation IR in strict SSA until atomic lowering.  Its equivalent
`LiveRangeEdit` operation must therefore insert real copy definitions at split
boundaries, place merge phis at the pruned iterated dominance frontier, and
rename uses along the dominator tree.  Those copy and phi results are ordinary
machine values and live intervals.  They are not home states, hard-colored
fragments, or a reason to bypass the base work queue.

The current Step 30 checkpoint has established the first production pieces of
this boundary.  `GreedyLiveRanges` owns the staged priority queue and eviction
cascades, physical occupancy is mutable, evicted ranges return to the queue,
and stale queue entries are rejected against the range's current stage.
Split plans now contain topology only.  A separate function-lifetime `Spiller`
owns `RootHomePlan`, chooses stack, State-MemorySSA, or rematerialization homes,
verifies those choices, and performs the corresponding private allocation-IR
edit.  The old `DeferredRound`, symbolic fragment reservations, and hard child
colors have been removed from the production allocation session.  A range for
which both split stages make no progress now reaches `Spill`; the spiller
materializes each concrete use and removes the exhausted source interval.

This is an intermediate checkpoint, not completion of the normative design.
A successful partial split still sends its moved complement directly to the
spiller instead of returning an unmaterialized remainder interval to the work
queue.  `JointAllocationSession` also remains as the base driver.  The next
slice must give that remainder its own machine value and exact live range,
requeue every split product, and invoke the spiller only after that remainder
itself fails assignment and both split stages.  Once that path is in place,
the remaining session/publication protocol can be replaced by the base driver
and one final allocation-IR-to-MIR rewrite.

Step 30c establishes the strict-SSA edit substrate without enabling it in the
production split path.  Allocation IR now has a first-class synthetic `Copy`
operation and synthetic merge-phi rows with stable VReg identities.  Copy
affinities, target lowering, incremental def/use deltas, full liveness rebuild,
and final strict-SSA MIR materialization all see the same values.  A diamond
regression inserts one copy on each arm and one merge phi, rewrites the joined
use, and requires incremental liveness to equal an independent reconstruction.
The next slice is the actual `SplitEditor`: legal cut placement, pruned-IDF phi
insertion, dominator renaming, child/root-use ownership, and work-queue stages.

### Legacy allocation algorithm

The implementation below records the joint allocator being removed.  It is
historical context and is not the design contract for new work.

The allocator operates on immutable root liveness and creates allocation units
without mutating MIR:

1. Build exact block liveness, sparse root intervals, loop/SCC nesting, use
   positions, constraint masks, and home recipes once.  Frequency and cost are
   annotations; they never alter semantic liveness.
2. Build each root's canonical live-range topology independently of physical
   occupancy.  Joint coloring then places every original and synthetic range
   against CFG-sparse interval unions, so branch-exclusive ranges can share a
   register without becoming adjacent in one layout-linear interval.
3. A coloring failure names the blocked definition and every resident region
   covering it. For each candidate and physical register, grow the maximal
   definition-connected free prefix over exact live-range CFG edges. Sibling
   paths may end at different occupancy cuts; those cuts form one register-
   specific frontier and are split in one transaction. Frontiers from
   different colors are never flattened together. Partition the union of
   moved uses among earliest dominating instruction uses; phi-edge entries and
   loop-reentry uses which dominate any cut become exact materializations.
   Select the minimum proved home cost and return every resulting machine
   value to the same joint allocator. The selected frontier color is part of
   the retained fragment decision, not a disposable affinity: split mutation
   changes the retained use ownership and register-region metadata together,
   and the persistent session revalidates the shortened sparse range against
   the updated matrix before restoring that exact color. If another fragment
   published in the same round now occupies it, the retained fragment stays
   unassigned and returns to ordinary coloring rather than overlapping the
   matrix.

   The moved multi-use regions are colored in the same round even though their
   synthetic VRegs do not exist yet. An epoch-marked sparse liveness projector
   maps each planned definition and exact use subset through the normalized
   CFG. Synthetic definitions reserve from the first unused sequence in their
   immutable insertion-anchor zone, so the later exact range is contained by
   the symbolic range without predicting how many recipe instructions will be
   emitted. The selected colors are inserted in the ordinary interval matrix
   as immutable planned occupancy. Subsequent original values and sibling
   split plans therefore see those ranges; mutually exclusive child regions
   may still reserve the same color because their CFG-sparse ranges do not
   overlap.

   At the publication boundary, planned occupancy is removed before fixed
   reservations and exact liveness change. Split mutation creates the real
   VRegs, carries each selected color in register-region ownership metadata,
   and the session inserts the exact rebuilt ranges into those colors. A new
   target constraint or fixed interval may invalidate the symbolic choice; in
   that case only that child remains pending for ordinary allocation. Updating
   allocation facts or installing real assignments while planned occupancy is
   live is a state-machine error. A value may consequently own any finite
   number of disjoint register regions without a side table that ordinary
   allocation can ignore.

   Multiple machine regions of that value may also remain deferred in one
   transaction. Their immutable root-use ownership must be disjoint. One
   root-round accumulator prices the union of their entry uses under the exact
   per-use MemorySSA/rematerialization alternatives and one shared stack-home
   creation cost. Candidate evaluation extends the additive accumulator only
   with its own entries; it does not repartition earlier plans. When the round
   closes, all root entries are grouped once and concrete homes are selected
   once, so a later entry may legitimately switch an earlier entry between a
   recipe and the now-amortized stack home. Publication independently rebuilds
   the accumulator from the deferred plans and unconditionally requires exact
   entry ownership, stack existence, and additive totals before mutation. The
   exact partition constructor then requires complete home coverage and cost
   identity; exhaustive verification also compares every concrete home.
4. Solve stack availability as sparse SSA dataflow over the selected home
   demands.  Place explicit stores at the latest legal dominating points or
   predecessor edges while the source location is available.  Materialize
   store, reload, state, and pure-recipe operations in the allocation IR.  All
   resulting def-to-store, reload-to-use, and recipe-intermediate ranges return
   to the ordinary allocation queue; none receives an invented scratch
   register after allocation.
5. Freeze `AllocationResult` only after every register-resident original and
   synthetic machine value has a physical assignment. Phi sources and
   destinations resolved directly through stack/immediate out-of-SSA locations
   are explicit exceptions, not phantom register ranges. Then lower the
   allocation IR, rename exact instruction and phi-edge uses, and construct the
   complete edge parallel-copy plan. Rewrite never consults spill weights,
   rejected candidates, or allocator caches.
6. Independently rebuild MIR liveness, physical interference, stack reaching
   definitions, MemorySSA recipe versions, fixed-register constraints, and
   edge parallel copies.  Failure is a producer bug, not a request to retry
   with another allocator or a more conservative global spill.

Termination follows from exact use ownership, not an iteration cutoff.  Define
the progress tuple as the sum of pairwise co-resident root uses in every
register region, then the number of original-register uses, then the total
number of register uses.  Every accepted split decreases this tuple
lexicographically.  A root may move all uses once from its original definition
to a later explicit transition; an existing synthetic region may not recreate
the same use set at the same immutable entry use.  Otherwise a split creates
strictly smaller disjoint regions or exact materialized uses, and regions never
merge.  Dead replaced reload/recipe DAGs are removed without compacting
surviving allocation-session identities, so they cannot accumulate as
artificial fixed pressure or invalidate persistent indexes.  Stack-home
placement is a monotone sparse dataflow problem.  MIR is
materialized once after this finite process; there is no unbounded production-
MIR rewrite/reanalyse loop.

### Machine-independent and HDL-specific responsibilities

The ordinary compiler responsibilities stay ordinary: exact CFG liveness,
SSA dominance, interval interference, register constraints, splitting,
recoloring, stack-slot coloring, and out-of-SSA parallel copies.  Celox does
not replace them with HDL-width tags or a branchless linear live range.

The HDL-specific input is limited to home choice and scale:

- a machine VReg has only target-relevant 32- or 64-bit semantics; arbitrary
  RTL widths remain in SIR/StateSSA metadata;
- a SimState home is a versioned MemorySSA recipe, possibly a multi-load
  shift/mask/merge DAG, proved separately at every selected transition;
- very large mutually exclusive RTL paths require CFG-sparse segments and
  near-linear storage rather than a layout-linear interval or all-pairs graph;
- fused comb/FF functions may expose a register value across a phase boundary,
  but that is an ordinary live region, not an implicit memory home.

### Implementation and test slices

Each retained slice ends at a verified representation boundary:

1. Introduce the immutable-anchor allocation IR and exact liveness for its
   original plus synthetic machine values.  Test def-to-store, reload-to-use,
   multi-step recipe DAGs, phi edges, and stable input-MIR identity.
2. Replace Boolean stack-home accounting with explicit stack definitions,
   reload demands, and an independent all-path reaching-definition verifier;
   enqueue every resulting synthetic range.
3. Expand every proposed home and register entry into explicit machine values,
   then recompute exact liveness; old physical assignments become affinities.
4. Jointly allocate every original and synthetic range.  Return an exact split
   obligation rather than finalizing a hidden scratch register.
5. Resolve split obligations into reachable CFG/dominance regions and homes,
   eliminate replaced synthetic DAGs, and rerun joint allocation to its proved
   fixed point.  Test diamonds, loops, stack-backed prefixes, multiple regions,
   and termination.
6. Normalize the completed solver state into exact per-use locations and lower
   one strict-SSA result atomically. Verify exact source-MIR instruction
   identity, definition dominance, recipe DAG edges, phi source/destination
   locations, independently rebuilt physical liveness, and the complete
   out-of-SSA parallel-copy plan before publication. **Complete in the
   diagnostic path.**
7. Split live ranges at fixed/clobber boundaries, rebuild mandatory masks from
   rewritten machine operands, coalesce copy/phi affinities transactionally,
   and color exact stack-home interference. **Complete in the diagnostic
   path.**
8. Replace production W/S only after differential MIR execution and the exact
   Heliodor Linux marker pass.  Measure code generation and generated-code
   execution separately; use non-LTO builds during iteration and a final
   release/LTO gate only at acceptance.

Compile-time tuning inside the diagnostic allocator is not a substitute for
these slices.  In particular, changing conflict-container order, map
thresholds, or per-register projection caches is out of scope unless a
completed architectural slice still fails its complexity contract.

The first retained implementation slice provided the allocation IR and
shared liveness boundary.  Original MIR instructions and phis are represented
by immutable anchors; synthetic stack stores, reloads, and recipe nodes receive
checked machine-value definitions.  Both representations use the exact same
CFG-sparse live-interval construction and independent equation verifier.  The
later retained slices connect home selection, recursive region allocation, and
atomic MIR lowering while leaving production code generation on the interim
allocator below.

Explicit synthetic stack operations now also have an independent sparse
all-path verifier.  It builds Boolean SSA only for homes with reload demands,
places AND meets through iterated dominance frontiers, and respects exact
operation order and normalized edge isolation.  Stores on every join arm
establish a home; a missing arm or a store after the reload does not.  Later
slices return every resulting machine value to joint allocation.

Allocator-selected homes now expand into that allocation IR without changing
production MIR.  A selected stack home has an explicit store and per-use
reload definitions; state and rematerialization choices become their exact
recipe DAGs; and the entry of a split register region becomes one synthetic
SSA definition shared by all uses in that region.  Existing physical-register
assignments are retained only as preferences.  Expansion then proves every
stack reload, recomputes exact liveness for every original and synthetic
machine value, and checks that each rewritten use is owned by its replacement
interval.  Immutable input-MIR use anchors and their shifted allocation-IR
positions are stored separately, including phi-edge exit slots.  The joint
allocator enqueues those recomputed intervals together and permits any finite
number of register regions; old diagnostic assignments are only affinities,
not a complete physical allocation.

The recomputed intervals now feed one joint allocation problem.  Every
machine definition, including original values, stack/reload ranges, and every
recipe intermediate, receives a stable allocation identity.  Retained root
ranges carry their exact root-use subset and an optional old-register
affinity; all other ranges are fixed transition values.  Coloring walks
definitions in dominator-tree order against the sparse physical interval
unions, so mutually exclusive CFG arms remain non-interfering.  A completed
assignment is independently rebuilt in a fresh matrix.  If no register is
available, the result contains every per-register resident conflict and all
root regions which may legally be split.  Pressure involving only fixed
transition ranges is rejected as a producer error rather than hidden behind a
scratch register.

Split obligations now close that joint-allocation loop.  A candidate must
cover the blocked definition.  Its exact sparse segments supply only live
exit-to-entry CFG edges, so traversal moves the reachable suffix without
claiming a sibling arm.  Dominance partitions the suffix into independently
entered regions; a backedge which revisits the cut includes next-iteration
pre-cut uses, but those uses are materialized singly because their static site
dominates the cut.  Phi-edge entry uses are also singletons until synthetic
phis are part of atomic lowering.  Stack selection creates one identified
definition-to-store use, which remains fixed while ordinary root uses remain
splittable.

Applying a plan mutates a clone of the allocation IR, rewrites immutable exact
use anchors, removes unreferenced region metadata and dead pure synthetic
DAGs, compacts their value/instruction identities, reruns the all-path stack
proof and exact liveness, and rebuilds the joint problem.  Publication requires
the ownership progress tuple to decrease. The resulting fixed-point allocator
passes focused synthetic-pressure, sibling-arm, loop-reentry, partial-stack,
and repeated-entry tests.

The completed result now lowers exactly once into a private `MFunction`.
Original instructions are compared against full immutable snapshots, including
opcode, width, immediate, and operands. Synthetic stack, state, constant, and
pure-recipe operations use one shared width-explicit MIR mapping. The lowered
function is accepted only when canonical MIR verification and an independent
physical-liveness rebuild reproduce the allocation problem exactly.

Phi boundaries are locations rather than forced simultaneous register ranges.
A stack/immediate phi source remains in the semantic phi row but is excluded
from predecessor-exit register liveness and becomes an exact destination-
qualified out-of-SSA source. A stack-resident phi destination is likewise
defined directly by every incoming parallel copy instead of becoming a
register definition followed by a store. Nontrivial edge recipes materialize
to an explicit edge-local stack home; all recipe intermediates still enter
joint allocation. This is required for functions with more phi rows on one
edge than physical registers. The resulting `AssignmentMap` and complete SSA
destruction plan are independently verified. Production code generation
remains on the interim allocator until the completed diagnostic result passes
differential execution and the Linux acceptance gate.

Target constraints no longer pin an unsplit long-lived VReg. Before home
selection, every fixed-operand or clobbering instruction receives an explicit
SSA permutation boundary containing the complete live set. The allocation
entry point permits more than K rows at this pre-spill boundary; those rows
are ordinary roots which may select stack/state/rematerialized homes and
re-enter the joint fixed point. This localizes a legacy shift's RCX
requirement and a divide's RAX/RDX exclusion to the representative spanning
that machine boundary.

After every allocation-IR rewrite, target facts are rebuilt from the immutable
opcode snapshot and the current operand row. A fixed use therefore constrains
the actual reload or recipe result consumed by the instruction, not a stale
source VReg. Clobber exclusions apply only when an exact sparse segment covers
both the instruction-use and instruction-definition slots. Mandatory masks
are checked during coloring and independently during result verification.
Mov and register-resident phi edges form a weighted affinity graph. Greedy
color choice consults already assigned neighbors, then a conservative
post-color pass removes both endpoints from the sparse interval matrix and
publishes a common color only if allowed masks, exact interference, and total
incident affinity weight all improve.

Stack homes are then analyzed as a separate location-level strict-SSA program,
not approximated from home counts or final reload positions. An explicit stack
store or stack-resident phi defines one home; stack reloads and direct
phi-edge stack locations use it. These facts retain current allocation-IR
instruction positions, so their block slots are required to match the machine-
value liveness layout exactly. Location-only phi-edge uses enter the same sparse
CFG equations and dominance verifier without inventing register phi results.

The resulting stack intervals are colored in definition/dominator order by a
dynamically growing sparse interval matrix. A new 64-bit frame color is created
only when every existing color interferes on an exact CFG segment. The final
home-to-slot map is rebuilt from scratch in a second matrix before concrete
offsets are assigned once. Thus mutually exclusive homes may share an offset,
while overlapping homes and simultaneous stack phi destinations cannot.
Production remains unchanged until the completed allocator passes its semantic
and execution gates.

## Interim allocator architecture

The techniques below describe the current implementation and solve different
subproblems in one fixed order. They are not competing allocators and there is
no spill/color retry loop. This order is retained here for diagnosis and
migration; it must not be copied as the final implementation where it omits
the authoritative pressure-region input/output relations.

```text
canonical strict-SSA MIR
  -> CFG and branch-edge normalization
  -> constraint-marker construction
  -> pressure-aware scheduling
  -> conventional-SSA normalization for existing phis
  -> global next-use and loop analysis
  -> Braun--Hack spill placement (W/S states and edge coupling)
  -> SSA reconstruction and dead-definition elimination
  -> spill-home and pressure proofs (maximum <= K)
  -> post-spill full-live Perm boundaries
  -> CFG/dominance renormalization and Perm proof
  -> implicit chordal SSA coloring
  -> phi-aware color preference
  -> SSA destruction and parallel-copy resolution
  -> final allocation proof
```

The relationship between the techniques is deliberately one-way:

| Technique | Problem it solves | Contract handed to the next phase |
| --- | --- | --- |
| CFG normalization | gives every branch edge a legal insertion point | edge-local copies and spills cannot execute on the wrong arm |
| pressure scheduling | removes pressure caused only by a poor order of independent instructions | equivalent MIR with pressure no greater than the input order |
| Method-I CSSA | makes phi-congruence members non-interfering | one sound spill home can represent each congruence class |
| global next use and Braun--Hack MIN | selects residents and places stores/reloads without a color retry | a finite spill plan whose reconstructed pressure is at most `K` |
| pruned-IDF reconstruction | restores strict SSA after the planned splits | fresh dominating representatives and no dead reload/phi web |
| late full-live Perm | isolates fixed-register and clobber constraints from global coloring | at-most-`K` components with a proved local perfect matching |
| chordal SSA coloring | assigns registers to the already spill-complete SSA graph | a total physical assignment; it never requests more spilling |
| SSA destruction | lowers phi/Perm semantics after colors and homes are fixed | verified edge-local parallel copies ready for encoding |

Scheduling and spilling are therefore complementary, not alternative
allocators: scheduling removes avoidable pressure once, while MIN handles the
remaining inherent pressure.  CSSA is a precondition of home formation, Perm
is a post-spill construction for machine constraints, and coloring only assigns
the graph proved feasible by those earlier phases.

### 1. CFG normalization

All outgoing edges of a branch receive dedicated one-predecessor/one-successor
edge blocks before any phase which may insert edge code.  This is stronger than
critical-edge splitting: it prevents code for one branch arm from running on
the other arm even when the successor originally had one predecessor.  Phi
sources are rewritten to the edge block.  IDs use checked `u32`/`usize` values;
there is no packed-index or CFG-size limit.

### 2. Machine constraints and late Perm boundaries

Repeated use of one physical color as a precoloring is the precoloring-extension
problem.  Pressure `<= K` alone does not make ordinary greedy chordal coloring
succeed.  A one-use fixed copy alone does not solve this problem either.

Before scheduling, fixed operands/results and physical clobbers are recorded as
immovable markers.  MIN pins instruction operands and reserves
`K - |clobbers|` for values live through a clobber.  It does not reserve a
register globally and does not insert fixed-use copies.

After spill reconstruction proves pressure `<= K`, the allocator applies the
full-live construction from Section 6 of *Towards Register Allocation for
Programs in SSA-form*.  Immediately before every marker it inserts a
single-predecessor multi-row phi/Perm containing every value register-live at
that point.  Dominated uses, including the constrained instruction, use fresh
Perm results; the appropriate results are precolored.  The boundary completely
disconnects the interference graph on both sides.

Materializing Perm after spilling preserves the proof while bounding its size:
a memory-resident value has no register live range across the marker and its
later reload/rematerialization is already a fresh definition.  Thus the full
post-spill set has at most `K` rows, instead of cloning an arbitrarily large
pre-spill live set.  The verifier proves row completeness, one-to-one
source/result coverage, renaming dominance, unique precolors per component,
and clobber exclusions.  CFG, dominators, frontiers, and loops are recomputed
after materialization.

At a Perm, its at-most-`K` results are assigned together by a local bipartite
matching between rows and physical colors.  Fixed operands/results remove all
but their required color; a value live through the constrained instruction
excludes every clobbered color; other rows admit the whole register class.
Already-colored sources provide only matching costs/preferences.  This local
matching is the constructive proof that the new component can start; arbitrary
global precolor-first greedy coloring is not used.  A missing perfect matching
is a constraint-pressure verifier failure.

### 3. Pressure-aware scheduling

Scheduling removes pressure caused by instruction order, not inherent
pressure.  Pure regions are def-use DAGs. Constant-address loads and stores
participate in the same DAG: byte-granular RAW, WAR, and WAW chains preserve
the order of overlapping accesses, while disjoint accesses may move nearer to
their uses. Dynamic/pointer accesses, releases, memory copies, control flow,
unknown memory effects, and constraint markers remain barriers. A priority
queue and incremental ready/dependency counts avoid rescanning the whole ready
set or block suffix. A schedule is accepted only when dependency verification
passes and exact high-water pressure does not increase. It runs once before
spilling, with no schedule/spill feedback loop.

The bottom-up ready queue uses the target register capacity `K`, rather than an
unbounded pressure-only or dependency-only priority. While the current live set
and the longest-path candidate both project to at most `K`, longest paths to the
region exit and from the region entry expose independent machine work. At the
capacity boundary, the candidate with the smallest immediate live-pressure
delta wins. Thus the scheduler keeps an instruction-level-parallelism window
which the target can hold, but does not create an arbitrarily large live set
for the spill planner to repair. All MIR opcodes, including the 32-bit ALU
forms, are classified explicitly as movable or as barriers.

### 4. Conventional SSA before spill-home formation

Braun--Hack Section 4.4 assigns one spill home to a whole phi-congruence class
and explicitly requires conventional SSA (CSSA): no two members of a class may
interfere.  Strict SSA alone does not imply this after copy propagation or code
motion.

The correctness baseline is Sreedhar Method I.  Each existing
`d = phi(s1, ..., sn)` is rewritten so fresh edge copies `s'i = si` feed a
fresh result `d'`, followed by an entry copy `d = d'`.  The already-normalized
edge blocks make the source copies edge-local.  A streaming liveness verifier
then proves the semantic condition for every congruence class; it does not trust
only the syntactic shape.  Method-III-style copy virtualization is a later
optimization and is legal only when the same verifier still passes.

Reload-reconstruction phis are created after spill homes have been fixed and
cannot merge two existing homes.  They are versions of one logical value.

### 5. Global next-use and loop analysis

The Braun--Hack analysis maps each live logical value to its closest CFG-global
next-use distance; joins take the minimum and loop-exit edges receive a large
weight.  Per-block use occurrences are stored once in a flat index and queried
by binary search or monotone cursor, never by suffix rescanning.  The same CFG
analysis supplies a loop tree, loop uses, and maximum loop pressure without an
edge-times-loop or nested-loop-times-instruction scan.

The same backward worklist computes a separate after-phi anticipatability
fact.  Unlike liveness or minimum next use, successor facts are intersected:
a value is anticipated only when every continuation uses it before an ordinary
definition replaces the SSA name.  Phi destinations are killed at the
successor boundary and their exact sources are generated on the corresponding
incoming edges.  An independently reconstructed equation verifier checks this
must-use result for every block; a one-arm use therefore cannot justify
extending a live range through the branch head.

Loop use sets are not copied into every ancestor region.  Each syntactic use is
attached once to its innermost natural-loop or irreducible-SCC region.  An
iterative Euler numbering makes every region subtree an interval, and one flat
index stores the direct-region positions for each VReg.  At a region entry,
`used(value, region)` is answered by a binary search for a position in that
interval.  Only the scalar maximum pressure is propagated bottom-up.  Thus a
nesting chain of depth `D` does not materialize `D` copies of every inner use:
storage is linear in CFG regions, VRegs, and direct use-region occurrences, and
hot/cold queries are performed only for values live at an actual region entry.

### 6. Braun--Hack spill placement

Spill placement operates on logical values without mutating MIR.  In reverse
postorder it computes `W_entry`, inserts deferred edge coupling, and runs MIN,
evicting the unpinned value with furthest global next use until `|W| <= K`.
For an edge `P -> B`, coupling reloads `W_entry[B] - W_exit[P]` and spills
`(S_entry[B] - S_exit[P]) intersect W_exit[P]`; backedges are coupled after
their predecessor state becomes available.

An ordinary join no longer fills spare `W_entry` capacity merely because a
value is resident on some predecessor.  For each such candidate, keeping it is
charged for reload/rematerialization on missing incoming edges, while dropping
it is charged for required home-creation stores and for a later reload only
when anticipatability proves that every continuation uses it.  Positive
avoided-cost candidates compete by loop-exit distance and
avoided-cost/live-range-span density.  Blocks with one predecessor inherit the
translated `W_exit` unchanged; only a real join performs edge reconciliation.

`S` means that one valid home exists on every root-to-point path.  A resident
value inherits a home only from the intersection of predecessor `S_exit`
states.  CSSA permits one home per original phi-congruence class without a
memory-to-memory phi copy.  Home creation, edge translation, and reload
dominance are explicit verifier obligations.  Coloring failure never requests
additional spilling.

### 7. SSA reconstruction

Each planned reload gets a fresh VReg.  Uses are renamed to the nearest
dominating definition and pruned iterated dominance frontiers receive the
needed phis.  This is a separate Sastry--Ju-style reconstruction phase, not an
opportunistic part of MIN.  A backwards use mark removes dead reloads, dead
Perm rows, and cyclic dead phi webs before the next phase.

After renaming, reconstruction tail-merges identical reload-only coupling
bundles which enter the same successor.  Equality includes the logical value,
spill home, and complete immediate, stack, or versioned-state recipe; the
bundle must be the exact suffix before an unconditional edge jump, and all
affected phi rows must collapse consistently.  Paths which already keep the
values resident continue to enter the merge directly.  This shares static
reload code and reconstruction-phi inputs without moving a reload onto a path
which did not previously execute it.  Because the transformation creates real
blocks after the allocation graph was frozen, the normalized CFG is rebuilt
once for the independent reload/pressure proofs, Perm construction, and
coloring.  It does not rerun spill planning.

### 8. Pressure and home verification

An independent forward/backward proof recomputes edge-sensitive liveness and
checks general pressure, pinned operands, fixed-color multiplicity, and
live-through clobber capacity at every point.  Each non-rematerialized reload
must be dominated on every path by a store to the same home.  Failure identifies
a producer bug and never triggers a retry, cap, fallback allocator, or expected
panic path.

### 9. Implicit chordal coloring

Once pressure is at most `K`, the SSA interference graph is `K`-colorable.  The
allocator uses the dominance-derived perfect elimination order from the SSA
coloring algorithm.  It scans blocks in dominance order, tracks only colors
currently live, releases last local uses which are not live-out, and uses a
dense physical-color forbidden mask per active VReg.  It does not retain a live
set per instruction and does not build an explicit interference graph.

Perm destinations receive the local matching selected at their boundary before
the component's ordinary definitions are colored.  This is distinct from
precoloring every constrained node in the whole function up front, which would
reintroduce the precoloring-extension problem.

Phi colors are preferences, not graph-node merging.  A separate verifier checks
the perfect-elimination property and the completed assignment's liveness,
fixed-register, and clobber constraints.

Definitions also carry ordinary x86 two-address affinities. A destination
prefers a dying source color for moves, unary operations, immediate forms, and
the appropriate operand of arithmetic/select instructions, but only after the
active-color, fixed-register, and clobber proofs say that color is available.
This reduces avoidable moves without changing coloring feasibility.

### 10. SSA destruction

Phi/Perm rows become edge-local parallel copies. Identity rows emit no code;
acyclic rows are drained in dependency order, and each cycle is broken with one
temporary while preserving fanout. Resolution handles register, stack, and
64-bit immediate sources, including stack-to-stack copies, and preserves
simultaneous-copy semantics. The emitter runs a copy plan only on the selected
branch edge. Copy-free fallthrough block chains share a machine-code label
instead of receiving padding instructions. Dead rows are absent before
resolution.

Within the interim implementation these are phase boundaries, not suggestions.
Each has a verifier for the
intended IR; no phase weakens a contract merely to accept an existing producer.

## Phase data model and APIs

The implementation uses the following conceptual data types.  Exact Rust field
layout may differ, but their ownership and invariants may not.

```text
NormalizedCfg
  block_index: BlockId -> usize
  predecessors / successors
  dominator_tree / dominance_frontier
  loop_tree

ConstraintModel
  fixed_uses: ProgramPoint -> [(operand, PhysReg)]
  clobbers:   ProgramPoint -> PhysRegSet

CssaInfo
  congruence_home: VReg -> SpillHome
  nontrivial_members: SpillHome -> [VReg]

PermModel
  boundaries: BlockId -> [PermRow]
  rows: source VReg, destination VReg, allowed-color mask
  local_matching: destination VReg -> PhysReg

NextUseAnalysis
  entry / exit: BlockId -> (LogicalValue -> distance)
  block_max_pressure
  loop_max_pressure
  used_in_loop

SpillState
  w_entry / w_exit: BlockId -> Set<LogicalValue>
  s_entry / s_exit: BlockId -> Set<LogicalValue>

SpillPlan
  edge_ops: EdgeId -> [Spill | Reload]
  point_ops: ProgramPoint -> [Spill | Reload]
  homes: PhiCongruenceClass -> SpillHome

ReconstructionResult
  strict SSA MFunction
  representative: (LogicalValue, ProgramPoint) -> VReg

ColoringResult
  VReg -> PhysReg
  edge parallel copies
  spill frame layout
```

`LogicalValue` names the value manipulated by MIN before reconstruction.  A
fresh VReg produced by a reload is a new SSA representative of that logical
value; it is not a new value eligible for an independent spill decision.
Keeping these identities separate prevents the exponential reload-respilling
behavior of the rejected implementation.

Logical values use the original dense VReg number directly; the implementation
must not allocate a singleton `Vec` or hash entry per logical value.  Frame
layout is computed once as `SpillHome -> offset` before reconstruction.  Every
store/load performs a constant-time lookup rather than rescanning the plan.

`ProgramPoint` refers to the normalized input MIR using `(BlockId,
instruction-index, side)` and remains stable while a `SpillPlan` is built.  The
planner never mutates MIR.  Plan materialization and SSA reconstruction consume
the plan and produce a new strict-SSA function atomically, so invalid
multiple-definition MIR is never exposed at a phase boundary.

The intended phase APIs are:

```text
normalize_cfg(&mut MFunction) -> NormalizedCfg
build_constraint_markers(&MFunction) -> ConstraintModel
schedule_for_pressure(&mut MFunction, &NormalizedCfg, &ConstraintModel)
normalize_to_cssa(&mut MFunction, &NormalizedCfg) -> CssaInfo
verify_cssa(&MFunction, &NormalizedCfg, &CssaInfo)
analyze_next_use(&MFunction, &NormalizedCfg) -> NextUseAnalysis
plan_spills(&MFunction, &NormalizedCfg, &NextUseAnalysis,
            &ConstraintModel, &CssaInfo, K) -> SpillPlan
verify_spill_plan_and_home_paths(&MFunction, &NormalizedCfg, &SpillPlan)
reconstruct_ssa(&MFunction, &NormalizedCfg, SpillPlan)
    -> ReconstructionResult
verify_pressure(&ReconstructionResult, &ConstraintModel, K)
materialize_perms(&mut ReconstructionResult, &ConstraintModel)
    -> (NormalizedCfg, PermModel)
verify_perms(&ReconstructionResult, &NormalizedCfg, &PermModel)
color_ssa(&ReconstructionResult, &NormalizedCfg, &PermModel, K)
    -> ColoringResult
verify_assignment(&ReconstructionResult, &ColoringResult)
destroy_ssa(&ReconstructionResult, ColoringResult) -> AllocatedFunction
verify_allocated(&AllocatedFunction)
```

Every mutating phase and verifier is exposed to the compilation driver as a
`Result`, even where the pseudocode omits it for readability.  Errors carry the
phase, stable rule identifier, block/edge, instruction, and involved values or
homes.  Invalid producer output, unsatisfiable machine constraints, and checked
identifier exhaustion become compilation diagnostics; they are not handled by
`panic!`, `unwrap`, a retry, or the old allocator.  A failed mutation is built
off to the side or rolled back so no partially invalid MIR escapes its phase.

### Constraint accounting

Machine constraints are not handled by a global `K-1` or `K-2` workaround.
Before spilling, the pressure model pins actual instruction operands and checks
live-through pressure at a clobber against the remaining colors.  After
spilling, full-live Perm boundaries split components and local matching assigns
their initial colors.  MIN may evict ordinary values but never a pinned operand.
Coloring failure is a verifier or allocator bug, not a request for another
spill iteration.

### Termination and complexity

There is no spill/color retry loop.  The only data-flow fixed point is global
next-use analysis on a finite-height lattice.  Distances are lexicographic
`(loop-region exits, instruction distance)` values, so no fixed magic weight can
be exceeded by a large function.  Reducible loops use their natural header;
multi-entry irreducible SCCs are explicit loop regions whose entry blocks use
the same region-use prioritization.  Spill placement is one RPO CFG sweep with
deferred backedge coupling.  Reconstruction is driven by definitions, uses,
and iterated dominance frontiers.  Coloring is one dominance-derived pass plus
an at-most-`K` matching at each Perm.

The target complexity is linear or near-linear in MIR size plus def-use/CFG
edges.  No step may clone a full live set for every instruction, rescan a whole
function per spilled value, or build an explicit all-pairs interference graph.

## Verification contract

Verification describes the intended IR, even when existing producers fail it.
When a check fails we decide whether the producer or the contract is wrong; we
do not weaken the verifier merely to accept existing output.

The register-allocation pipeline verifies all of the following:

- MIR is reachable strict SSA before and after every splitting pass;
- the normalized block index, predecessor/successor graph, dominator tree,
  dominance frontiers, natural-loop membership, and loop forest agree with MIR;
- every original phi congruence class is interference-free before homes form;
- next-use operand positions exactly match MIR and every entry/exit map satisfies
  the CFG, phi-edge, loop-exit, and block-transfer data-flow equations;
- every reload has a fresh definition and a same-home store on every incoming
  path unless it is rematerialized;
- phi sources are associated with their actual predecessor edge;
- register pressure after spilling is within the allocatable set;
- every Perm contains exactly the complete post-spill register-live set and its
  local color matching is total;
- fixed operands occupy their required register and values live across a
  clobber do not occupy a clobbered register;
- simultaneously live values never share a physical register;
- every encoded MIR use and definition has a physical assignment;
- the explicit SSA-destruction artifact contains exactly one correctly located
  row for every phi on every incoming edge; and
- edge parallel copies preserve simultaneous-copy semantics, including
  register, stack, and immediate cycles.

Phase-boundary verification is unconditional in debug and release builds.
`CELOX_SIR_VERIFY_PASSES=1` and `CELOX_MIR_VERIFY_PASSES=1` enable additional
per-optimizer-pass audits; neither is required for the boundaries above.

## Performance and migration gates

The allocator will be accepted by `scripts/run-heliodor-bench.sh gate`, not
only by small unit tests. The command and its external-process-free contract
fixtures are implemented; the replacement is not performance-qualified until
that fixed gate itself returns success. It is complete only when:

- allocation does not panic on large valid MIR;
- compilation and execution complete without an iteration or CFG-size cap;
- allocation time and inserted load/store counts are reported separately;
- `comb_observer`, native execution tests, and per-pass MIR verification pass;
  and
- the end-to-end Heliodor result is compared with `veryl-cc` under the same
  timeout and workload.

The old unified allocator is not a production selector or correctness
fallback.  Its source remains compiled only by unit tests while the remaining
differential fixture is migrated; a failure in the new allocator is a bug to
diagnose and fix.

## Implementation status

The frozen allocation pipeline is now the default `auto` implementation.  It
contains:

- dedicated insertion blocks for every branch edge, RPO layout, iterative
  dominator/loop/SCC construction, a fully checked normalized-CFG model, and no
  CFG-size or traversal-depth cap;
- dependency-verified, target-capacity-aware list scheduling with one backward
  liveness pass per block and indexed ready buckets rather than suffix or
  ready-set rescans;
- Method-I CSSA normalization and an independent semantic
  congruence-interference verifier;
- lexicographic next-use distance over natural-loop and irreducible-SCC regions,
  with no fixed loop-distance constant, one block/instruction summary pass,
  Euler-interval/flat-index nested-region queries, a complete Bellman-equation
  verifier, CFG-intersection anticipatability with an independent equation
  verifier, and the same priority at every irreducible-region entry;
- a Braun--Hack-style W/S spill plan with cost-aware true-join residency and an
  independent sparse-SSA all-path, same-home store/reload proof without a
  block-by-home state matrix;
- separate pruned-IDF SSA reconstruction, stack-slot precomputation,
  rematerialization, dead reload/cyclic-phi removal, and exact edge-reload tail
  sharing;
- post-reconstruction full-live Perm materialization, including pruned-IDF
  merge phis when a Perm splits only one CFG path, exact allowed-color masks,
  and local bipartite matching;
- dominance-order streaming chordal coloring without program-point live-set
  tables, explicit interference adjacency, or a spill/color retry loop; and
- explicit, independently verified SSA-destruction plans plus a final
  MIR/assignment/frame proof immediately before x86 encoding.

`auto` and `ssa` both use this allocator. `interval-diagnostic` builds and
verifies the complete replacement result but deliberately discards it;
`interval` publishes the atomically lowered replacement result. `unified` is
deliberately rejected by `CELOX_REGALLOC_IMPL`, and a failure never selects
another implementation.

The previously rejected iterative splitter expanded Heliodor `eval_comb` from
roughly 146,000 MIR instructions through 480,000, 1.1 million, 2.3 million, 4.7
million, and 9.5 million instructions.  The rejected early full-live Perm also
created about 2.3 million VReg identities from roughly 400,000 input VRegs.
Both measurements motivate the frozen late-Perm architecture; neither is a
reason to add an iteration, branchification, or CFG-size cap.

In the pre-whole-region diagnostic snapshot, the `test_soc_linux_boot`
compile-only run completed in about 30.6 seconds. The cost-directed CFG then
presented to `eval_comb` had
7,738 SIR/MIR blocks and 152,086 post-MIR-optimization instructions. Scheduling
reduced its measured maximum straight-region pressure from 2,229 to 2,024;
allocation produced a 79,216-byte spill frame. SSA destruction saw
33,697 rows, of which 23,587 are identities and 10,110 require code (including
1,442 cycle breaks). These historical figures motivated the later StateSSA,
reload-recipe, and placement work; they are not the current performance result.

The current non-LTO qualification passes all 715 library tests, 60 non-ignored
native-testbench tests, and 9 native/Cranelift/Wasm counter tests. Two clean
full Heliodor runs reached normal power-down at the exact Veryl reference
`cy=9ae070 x3=aa pass=1`, taking `198.235 s` and `184.652 s`. The paired
Veryl-CC run took `76.446 s`. The subsequent fixed gate built Celox commit
`e917489e` with release/LTO and measured `178.223 s`, versus `68.409 s` for
Veryl-CC. Both exact markers and semantic checks passed, but the remaining
`2.605x` gap failed the gate's no-slower performance condition. Full native
execution is therefore correct and complete while the throughput target
remains open.
That fixed-gate number includes compilation. The explicit non-LTO split measured
Celox at `40.450 s` compile and `137.675 s` execute, versus synchronous Veryl
AOT-C with an empty cache at `58.354 s` compile and `54.282 s` execute.
Allocator and emitted-runtime changes are now judged by the `2.536x` execution
gap, while allocation and full compiler latency remain in the separate compile
interval.
The earlier Celox `cy=9ab960` completions were invalidated by an ISel
wide-to-narrow canonicalization bug, not by the allocator.

The next retained throughput step added structural byte-granular MemorySSA to
native load GVN and made its physical write effects shared with allocator
reload analysis. This fixed an unsound path-scoped load invalidation at CFG
joins and lets same-version `SimState` loads reuse a dominating value even when
the MIR live range grows: the allocator can reconstruct that value from its
independently checked state home instead of forcing it to remain in a register.
Exact sparse-commit and sparse-active metadata ranges no longer invalidate
unrelated homes. Indexed and pointer writes remain conservative; in particular,
`StoreIndexed` still invalidates every tracked byte on its base until a proved
index range is represented in MIR alias analysis.

An interleaved Step 8 / candidate / Step 8 non-LTO Linux measurement separated
compilation from generated-code execution. The adjacent Step 8 executions took
146.367 s and 146.216 s; the structural-MemorySSA candidate took 137.843 s, a
5.78% reduction from their mean. All three completed at
`cy=9ae070 x3=aa pass=1`. Their compile intervals were 40.186 s, 39.483 s, and
39.326 s respectively and are not part of that execution-time result.

The next alias-analysis step gives `StoreIndexed` an optional physical-state
write envelope. ISel derives it from the destination allocation for dynamic
value/mask stores and commits, and from the padded data/dirty/summary regions
for sparse writes. This is operation-level MemorySSA metadata, not a narrow
integer type on the index VReg; MIR register operations continue to distinguish
only target-relevant 32- and 64-bit semantics. A missing proof still clobbers
the whole base, while pointer writes and `SparseCommitWorklist` remain unknown.

Both global load GVN and reload reconstruction consume the same write-effect
range. On the exact Linux MIR this reduced loads of one repeatedly tested
selector from 56 to 32 and reduced the fused spill frame from 42,448 to 39,208
bytes without changing optimized SIR. The CPU-0 A--B--A executions were
144.622 s, 139.287 s, and 138.620 s, all at
`cy=9ae070 x3=aa pass=1`; the identical baselines vary more than the candidate's
1.65% advantage over their mean, so this establishes the structural result but
does not yet establish a runtime speedup. Their compile intervals were 59.225 s,
60.763 s, and 59.821 s and are kept separate from generated-code execution.

The next reconstruction step shares identical reload-only edge tails after
Braun--Hack coupling.  In the exact Linux MIR, four correlated case arms had
four static copies of the same seven reloads and seven five-input
reconstruction phis.  They now enter one shared seven-reload block and seven
two-input phis; the resident edge remains direct.  Taken paths still execute
seven reloads, so this is deliberately a static-code result rather than a
claim that spill placement is solved.  MemorySSA identities use stable block
IDs and per-block SimState-write ordinals, while trivial phi SCCs are
canonicalized only when they have one external reaching version.  The CPU-0
Step 12 / candidate / Step 12 execution intervals were 139.706 s, 137.092 s,
and 141.808 s, all at `cy=9ae070 x3=aa pass=1`; their compile intervals were
69.770 s, 63.295 s, and 66.919 s and remain a separate result.  A final
post-test candidate qualification also passed but took 70.140 s compile and
148.424 s execute, so neither timing improvement is considered established.

The following join-placement step uses CFG anticipatability and target
spill/reload costs instead of filling spare join registers from partially
resident values by next use alone.  The exact optimized SIR is unchanged.  In
the exact Linux MIR, the four correlated case arms no longer enter the shared
seven-reload block: those unconditional loads are delayed to the five later
regions which actually use six values, and the seventh is rematerialized as
zero.  A CPU-0 Step 13a / candidate / Step 13a measurement kept compilation
separate at 61.226 s, 61.512 s, and 60.899 s; generated-code execution took
137.664 s, 135.005 s, and 131.713 s.  Every run reached
`cy=9ae070 x3=aa pass=1`.  The candidate lies inside baseline variation, so
the placement result is retained structurally without claiming a measured
speedup.

The public allocator and chained native emitter now return structured errors,
failed public allocations leave their input MIR unchanged, and
completed-assignment verification is unconditional.  Internal default-SSA
mutators and verifiers return structured errors, fresh VReg/BlockId allocation
is checked, and the valid-input path contains no `panic!`, `assert!`, `expect`,
or `unwrap`.  The only remaining migration item is deleting the test-only
legacy source after its last differential fixture is expressed against the new
allocator.  That cleanup cannot add a retry, fallback, or size/iteration cap.
