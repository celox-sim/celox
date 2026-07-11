# Native register allocation

> **Status:** this document records the currently implemented interim SSA
> allocator and the failures that motivated its replacement. It is not the
> target architecture for new work. The normative fixed phase order, including
> verified scheduling, full-cut `PressureRegion`s, independently rebuilt
> `RegionalNextUse`, post-materialization normalization, final affinity,
> `ParallelCopyPlan`, edge lineage, and the executable Heliodor gate, is in
> [Decision-region architecture](./decision-region-architecture.md), sections
> 4--9. Where the two documents differ, that design is authoritative.

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
only by small unit tests. Until that command is implemented, the replacement
is not performance-qualified. It is complete only when:

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

## Interim implementation status

The frozen allocation pipeline is now the default `auto` implementation.  It
contains:

- dedicated insertion blocks for every branch edge, RPO layout, iterative
  dominator/loop/SCC construction, a fully checked normalized-CFG model, and no
  CFG-size or traversal-depth cap;
- dependency-verified pressure scheduling with one backward liveness pass per
  block and indexed ready buckets rather than suffix or ready-set rescans;
- Method-I CSSA normalization and an independent semantic
  congruence-interference verifier;
- lexicographic next-use distance over natural-loop and irreducible-SCC regions,
  with no fixed loop-distance constant, one block/instruction summary pass,
  Euler-interval/flat-index nested-region queries, a complete Bellman-equation
  verifier, and the same priority at every irreducible-region entry;
- a Braun--Hack-style W/S spill plan and an independent sparse-SSA all-path,
  same-home store/reload proof without a block-by-home state matrix;
- separate pruned-IDF SSA reconstruction, stack-slot precomputation,
  rematerialization, and dead reload/cyclic-phi removal;
- post-reconstruction full-live Perm materialization, including pruned-IDF
  merge phis when a Perm splits only one CFG path, exact allowed-color masks,
  and local bipartite matching;
- dominance-order streaming chordal coloring without program-point live-set
  tables, explicit interference adjacency, or a spill/color retry loop; and
- explicit, independently verified SSA-destruction plans plus a final
  MIR/assignment/frame proof immediately before x86 encoding.

`auto` and `ssa` both use this allocator.  `unified` is deliberately rejected
by `CELOX_REGALLOC_IMPL`, and a failure never selects another implementation.

The previously rejected iterative splitter expanded Heliodor `eval_comb` from
roughly 146,000 MIR instructions through 480,000, 1.1 million, 2.3 million, 4.7
million, and 9.5 million instructions.  The rejected early full-live Perm also
created about 2.3 million VReg identities from roughly 400,000 input VRegs.
Both measurements motivate the frozen late-Perm architecture; neither is a
reason to add an iteration, branchification, or CFG-size cap.

With SIR/MIR boundary verification and the new allocator's phase verifiers
enabled, the current `test_soc_linux_boot` compile-only run completes in about
30.6 seconds. The cost-directed CFG currently presented to `eval_comb` has
7,738 SIR/MIR blocks and 152,086 post-MIR-optimization instructions. Scheduling
reduces its measured maximum straight-region pressure from 2,229 to 2,024;
allocation then produces a 79,216-byte spill frame. SSA destruction sees
33,697 rows, of which 23,587 are identities and 10,110 require code (including
1,442 cycle breaks). These figures are diagnostics, not a performance pass.

The 252-test library suite, all 145 non-ignored `comb_observer` cases, the
16-test native suite with per-pass SIR/MIR auditing, and the native
control-preserving mux integration test pass. End-to-end Heliodor execution and
the same-condition `veryl-cc` comparison gate remain open. Celox has not had a
fast successful full Linux-boot run on this gate: prior status-0 Celox entries
were compile-only, and current 60-second executions are intentionally partial
diagnostic windows.

The public allocator and chained native emitter now return structured errors,
failed public allocations leave their input MIR unchanged, and
completed-assignment verification is unconditional.  Internal default-SSA
mutators and verifiers return structured errors, fresh VReg/BlockId allocation
is checked, and the valid-input path contains no `panic!`, `assert!`, `expect`,
or `unwrap`.  The only remaining migration item is deleting the test-only
legacy source after its last differential fixture is expressed against the new
allocator.  That cleanup cannot add a retry, fallback, or size/iteration cap.
