# Native register allocation

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

Go's production allocator is also used as an implementation reference for
machine constraints and edge shuffles, not as a source of compact identifiers:

- [Go compiler register allocator](https://go.googlesource.com/go.git/+/8b25a00e6d889c8a919922f747791478c8bdfe6f/src/cmd/compile/internal/ssa/regalloc.go)

`regalloc2` is not a dependency or design target.  Its large-function behavior
and compact internal index constraints do not meet Celox's requirements.

## Complete allocator architecture

The relevant techniques solve different problems.  They are composed in the
following fixed order; they are not alternative allocators and they do not run
in a retry loop:

```text
canonical SSA MIR
  -> critical-edge normalization
  -> machine-constraint legalization
  -> pressure-aware instruction scheduling
  -> global next-use and loop analysis
  -> Braun--Hack spill placement (W/S states and edge coupling)
  -> SSA reconstruction for inserted reload definitions
  -> pressure verification (maximum <= K)
  -> chordal SSA coloring
  -> phi-aware color preference/coalescing
  -> SSA destruction and parallel-copy resolution
  -> final allocation verification
```

### 1. CFG normalization

All critical edges are split before any phase which may insert edge code.  Phi
sources are rewritten to name the new edge block.  The result remains strict
SSA and every CFG edge has an unambiguous insertion point.  Block and edge IDs
use checked `u32` allocation; there is no compact-ID limit.

### 2. Machine-constraint legalization

Fixed-register operands and two-address requirements become explicit,
short-lived SSA copies.  Instruction clobbers remain explicit target facts.
No later phase may change the physical register of a VReg over only part of its
live range.  Legalization is verified as ordinary strict SSA MIR.

### 3. Pressure-aware scheduling

Scheduling removes pressure caused only by a poor instruction order; it cannot
remove pressure inherent in the program.  Within a scheduling region, a
def-use and memory-dependence DAG is scheduled with incremental top/bottom
pressure tracking.  Stores, release operations, control flow, and unknown
memory effects are ordering constraints rather than movable instructions.

The scheduler is accepted only if it preserves dependencies and does not raise
the exact high-water pressure of its region.  It runs once before spilling.
There is no schedule/spill feedback loop.  This corresponds to production
machine schedulers such as LLVM's pressure-tracking `ScheduleDAGMILive`.

### 4. Global next-use and loop analysis

The Braun--Hack analysis maps every live variable to its closest CFG-global
next-use distance.  Joins take the minimum distance.  Loop-exit edges receive a
large weight so uses inside a loop are preferred over uses after the loop.

The same pass builds the loop tree, identifies loop headers, records values
used in each loop, and computes each loop's maximum pressure.  Critical-edge
normalization is a precondition of this analysis.

### 5. Braun--Hack spill placement

Spill placement operates on logical variables before SSA reconstruction.  For
each block in reverse postorder it performs exactly the three steps from the
paper:

1. Compute `W_entry`, the variables required in registers at block entry.
   Normal blocks use the intersection/union of predecessor `W_exit` states and
   next-use order.  Loop headers use `usedInLoop` and loop maximum pressure as
   specified by `initLoopHeader`.
2. Insert edge coupling.  For predecessor `P` of `B`, reload
   `W_entry[B] - W_exit[P]` and spill
   `(S_entry[B] - S_exit[P]) intersect W_exit[P]`.  Unprocessed backedges are
   recorded and coupled when their predecessor is processed.
3. Run MIN through the block, evicting the unpinned variable with the furthest
   global next use until `|W| <= K`.

`S` obeys the paper's invariant: a variable is in `S` at a program point iff a
valid spill home exists on every path from the CFG root to that point.  Spill
slots are assigned per phi-congruence class so a spilled phi does not introduce
memory-to-memory copies.

This phase is one enhanced-liveness pass plus one CFG sweep.  Coloring failure
must never trigger more spilling.

### 6. SSA reconstruction

Spill placement temporarily creates additional definitions of a logical
variable at reloads.  A separate reconstruction phase restores strict SSA:

- every reload receives a fresh VReg;
- uses are renamed to the closest dominating definition;
- iterated dominance frontiers receive only the required new phi nodes; and
- dead reload definitions are discarded.

This is the reconstruction described by Braun--Hack using the Sastry--Ju
approach.  Trying to perform this renaming opportunistically inside MIN or edge
coupling is explicitly forbidden.

### 7. Pressure verification

After reconstruction, an independent verifier recomputes liveness and proves
that maximum pressure is at most the available register count at every program
point and edge.  It also checks that every reload is dominated by a valid spill
home.  Failure is a spiller bug; it does not cause another spill iteration.

### 8. Chordal SSA coloring

Once pressure is at most `K`, the SSA interference graph is `K`-colorable.  A
postorder walk of the dominance tree provides a perfect elimination order.
Coloring uses this order and implicit live sets, without building the full
interference graph and without spilling.  Fixed registers and clobbers restrict
the available color set.

### 9. Coalescing and SSA destruction

Phi coalescing is a color preference, not graph-node merging: sources and
destinations prefer the same color when legal, preserving chordality.  Finally,
phi nodes become edge-local parallel copies.  Copy resolution supports
register, stack, and immediate sources and is cycle-safe.

These are phase boundaries, not suggestions.  Each boundary has a concrete IR
contract and verifier; no phase defensively repairs another phase's output.

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
  reservations: short-lived, pinned precolored values

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

`ProgramPoint` refers to the normalized input MIR using `(BlockId,
instruction-index, side)` and remains stable while a `SpillPlan` is built.  The
planner never mutates MIR.  Plan materialization and SSA reconstruction consume
the plan and produce a new strict-SSA function atomically, so invalid
multiple-definition MIR is never exposed at a phase boundary.

The intended phase APIs are:

```text
normalize_cfg(&mut MFunction) -> NormalizedCfg
legalize_constraints(&mut MFunction, &NormalizedCfg) -> ConstraintModel
schedule_for_pressure(&mut MFunction, &NormalizedCfg, &ConstraintModel)
analyze_next_use(&MFunction, &NormalizedCfg) -> NextUseAnalysis
plan_spills(&MFunction, &NormalizedCfg, &NextUseAnalysis,
            &ConstraintModel, K) -> SpillPlan
reconstruct_ssa(&MFunction, &NormalizedCfg, SpillPlan)
    -> ReconstructionResult
verify_pressure(&ReconstructionResult, &ConstraintModel, K)
color_ssa(&ReconstructionResult, &ConstraintModel, K) -> ColoringResult
destroy_ssa(&ReconstructionResult, ColoringResult) -> AllocatedFunction
```

### Constraint accounting

Machine constraints are not handled by a `K-1` or `K-2` workaround.  Fixed
register copies and clobber reservations are pinned, precolored values in the
pressure model.  MIN may evict ordinary values around them but may never evict
a reservation.  The pressure verifier checks both total GPR pressure and the
availability of each required physical register.  Consequently coloring is
not expected to discover a constraint failure after spilling.

### Termination and complexity

There is no spill/color retry loop.  The only data-flow fixed point is global
next-use analysis on a finite-height lattice.  Spill placement is one RPO CFG
sweep, with deferred coupling for not-yet-processed backedges.  SSA
reconstruction is driven by definitions, uses, and iterated dominance
frontiers.  Coloring is one dominance-derived elimination/coloring pass.

The target complexity is linear or near-linear in MIR size plus def-use/CFG
edges.  No step may clone a full live set for every instruction, rescan a whole
function per spilled value, or build an explicit all-pairs interference graph.

## Verification contract

Verification describes the intended IR, even when existing producers fail it.
When a check fails we decide whether the producer or the contract is wrong; we
do not weaken the verifier merely to accept existing output.

The register-allocation pipeline verifies all of the following:

- MIR is strict SSA before and after every splitting pass.
- every reload has a fresh definition and a valid home;
- phi sources are associated with their actual predecessor edge;
- register pressure after spilling is within the allocatable set;
- fixed operands occupy their required register and values live across a
  clobber do not occupy a clobbered register;
- simultaneously live values never share a physical register;
- every MIR use and definition has an assigned location; and
- edge parallel copies preserve simultaneous-copy semantics, including cycles.

Verification is enabled in debug builds and can be forced in release builds
with `CELOX_REGALLOC_VERIFY=1`.

## Performance and migration gates

The allocator is evaluated on `scripts/run-heliodor-bench.sh`, not only on
small unit tests.  The migration is complete only when:

- allocation does not panic on large valid MIR;
- compilation and execution complete without an iteration or CFG-size cap;
- allocation time and inserted load/store counts are reported separately;
- `comb_observer`, native execution tests, and per-pass MIR verification pass;
  and
- the end-to-end Heliodor result is compared with `veryl-cc` under the same
  timeout and workload.

The old unified allocator may remain temporarily as an explicitly selected
diagnostic implementation, but it is not a correctness fallback: a failure in
the new allocator is a bug to diagnose and fix.

## Implementation status

The following parts are in the tree now:

- fixed-register uses are isolated behind fresh, one-use SSA copies before
  allocation;
- the final verifier independently recomputes liveness and checks local
  residency, fixed-register uses, clobbers, and edge-copy locations;
- edge homes record the exact program point from which their location is valid;
- the unified allocator is forbidden from changing an existing function-wide
  VReg assignment at a block boundary; and
- all identifiers remain `u32` or `usize` with checked allocation.
- spill-free functions are colored by the new SSA allocator in a
  dominance-compatible order without constructing an interference graph.

During migration, `CELOX_REGALLOC_IMPL` controls selection:

- `auto` (default) uses SSA coloring and temporarily routes functions which
  require spill placement to the unified allocator;
- `ssa` requires the new path, including fresh-SSA spill splitting and
  stack/immediate phi edge homes, and never silently falls back; and
- `unified` selects the old implementation for differential diagnosis.

The current forced-SSA spill path is an experimental whole-live-range splitter,
not the Braun--Hack algorithm above.  It is rejected as the production design
because its reloads become spill candidates in later iterations and large
functions grow superlinearly.  It will be removed when the `W/S` spiller and
SSA reconstruction phases land.  Until then, passing the verifier only means
the emitted allocation satisfies the current location model.

On the pinned Heliodor `test_soc_linux_boot` input, the first implementation
slice colors `apply_ff` (5,395 MIR instructions) entirely on the new path with
no spill frame.  The three larger evaluation functions currently report their
first spill requirement and use the migration path.  This split is observable
with `CELOX_REGALLOC_TIMING=1`; it is not inferred from compile success alone.

The rejected path passes small correctness regressions but, on Heliodor
`eval_comb`, repeatedly expands approximately 146,000 MIR instructions through
480,000, 1.1 million, 2.3 million, 4.7 million, and 9.5 million instructions.
This is evidence against its architecture, not a reason to add an iteration or
CFG-size cap.

Implementation of the replacement must follow the numbered phases above.  A
phase is not enabled by default until its verifier, focused CFG/loop tests, and
Heliodor compile-time and spill-count gates pass.
