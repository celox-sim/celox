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

## Required pipeline

1. **Machine-constraint legalization.** Fixed-register operands, clobbers, and
   two-address requirements become explicit short-lived SSA values or explicit
   forbidden-register facts.  The allocator never silently changes a value's
   physical register halfway through its live range.
2. **SSA spill placement and live-range splitting.** Pressure is reduced before
   coloring.  Every reload defines a fresh VReg and every rewritten use is
   dominated by that definition.  Phi operands are edge uses.  A critical edge
   is split before edge-specific code is inserted.
3. **SSA coloring.** After pressure is at most the number of allocatable
   registers, values are colored in a dominance-compatible perfect-elimination
   order.  The chordal SSA interference graph need not be materialized.
4. **Out of SSA.** Phi nodes become parallel copies after coloring.  Copies are
   attached to a specific predecessor edge and are resolved cycle-safely.

These are phase boundaries, not suggestions: each phase must produce valid MIR
which the next phase may assume without defensive repair.

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

The first SSA spill-placement implementation is now present.  It rewrites each
ordinary use to a fresh reload definition, assigns spilled phi sources and
destinations edge-specific memory homes, and batches pressure-driven splits in
one MIR traversal.  Final removal of the unified allocator remains migration
work.  Until that lands, passing the verifier means
the emitted allocation satisfies the current location model; it does not make
the unified algorithm the intended long-term design.

On the pinned Heliodor `test_soc_linux_boot` input, the first implementation
slice colors `apply_ff` (5,395 MIR instructions) entirely on the new path with
no spill frame.  The three larger evaluation functions currently report their
first spill requirement and use the migration path.  This split is observable
with `CELOX_REGALLOC_TIMING=1`; it is not inferred from compile success alone.

The forced SSA path passes the native execution, combinational-observer, and
32-way phi-pressure regressions.  On the large Heliodor `eval_comb` function it
still exceeds the 120-second compile-only measurement window, so it is not the
default for functions requiring spills.  The next performance step is a
Braun--Hack-style block-state spill planner which avoids repeatedly rebuilding
global liveness after each split batch; adding an iteration or CFG-size cap is
not an acceptable substitute.
