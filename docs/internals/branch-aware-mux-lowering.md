# Branch-aware mux lowering

Celox represents hardware selection as a `Mux`, but software simulation does
not always benefit from evaluating both inputs and selecting afterward.  The
legacy compiler currently has two opportunities to preserve control flow.
They share one profitability rule; they are not competing register allocation
workarounds. This document describes that legacy boundary, not the final
decision-region pipeline.

## Relationship between the two stages

1. **Cost-directed SLT lowering is the primary transform.**  While the source
   expression DAG is still available, the lowerer can put the exclusive part
   of each mux arm in separate SIR blocks.  A common root or direct operand is
   materialized once in the dominator.  If a non-trivial shared expression is
   deeper than that locally provable frontier, the conservative policy keeps
   the mux branchless instead of duplicating CSE work.  This is where
   case/decoder control should normally be preserved, before eager lowering has
   lengthened all arm live ranges.
2. **`BranchifyMux` is a post-lowering cleanup transform.** It considers muxes that remain
   after lowering and SIR optimization.  It may sink only pure, single-use arm
   definitions.  Shared definitions remain in the head block.  The transform
   uses the exact set of definitions that its legality repair will move when
   estimating cost, and charges values that must stay live across the new
   diamond.

An SLT mux already lowered to CFG is no longer a SIR `Mux`, so the cleanup pass
cannot branchify it a second time.  The cleanup pass exists for opportunities
that only become visible after SIR simplification, not as a replacement for
source-DAG lowering.

`BranchifyMux` is deliberately limited to locally proved, two-state regions.
It does not claim that every Mux should become a branch: four-state Muxes,
shared arm DAGs, impure operations, and transformations whose expected cost is
not positive remain dataflow. A future whole-DAG decision-region optimization
must use a verified rewrite plan rather than treating generic CFG/SSA
verification as a semantic proof.

## Local expected-cost decision

For true-arm work `T`, false-arm work `F`, and static true probability `p`, the
work avoided by replacing a select with a branch is

```text
saved = (1 - p) * T + p * F + select_cost
```

The work introduced is

```text
introduced = control
           + min(p, 1 - p) * mispredict_penalty
           + result_phi_copies
           + live_through_pressure
```

All arithmetic is performed with integer probability weights.  Conversion is
legal only when `saved > introduced`; equality stays as a mux.  Instruction,
phi, and live-through costs scale with the number of 64-bit chunks.  Arithmetic
such as multiplication and division is weighted by its lowered dynamic work,
not treated as one SIR instruction.

Without profile data, an equality-to-constant decoder condition is predicted
false (and inequality predicted true); other conditions use an even prior.
`EqWildcard` and `NeWildcard` use the same rule in 2-state mode.  This is a
branch-probability heuristic, not an arm-count or function-size threshold.
The prediction direction follows Ball and Larus, [*Branch Prediction for
Free*](https://doi.org/10.1145/155090.155119), PLDI 1993. The 20/80 weight is a
Celox engineering prior used in the absence of profile data; it is not a
semantic rule or a theorem from that paper and must ultimately be calibrated
against measured target behavior.

There is deliberately no global iteration, transformation-count, block-count,
or code-size cap.  A function may contain any number of profitable diamonds,
but every diamond must independently pay for its branch, expected
misprediction, merge copies, and downstream liveness cost.

## Legality

- Four-state muxes remain dataflow muxes.  A four-state `Mux` maps an X/Z
  condition to an all-X result, while the current SIR `Branch` tests only value
  bits; CFG conversion would therefore change behavior.
- Only pure arm instructions may move.  Stores, commits, runtime events, and
  capture events are never speculatively moved.
- `ForFold` is not a pure mux arm even when its capture-effect list is empty:
  it lowers to runtime loop control and includes non-progress `Error` exits.
- A load may move only when no intervening write or event can conflict with it.
- Definitions used by both arms or outside the candidate remain in their
  dominating block.
- A mux result used after the split is represented by a merge parameter.  A
  single immediate store may instead be distributed to the two arms when the
  mux result has no other use.

## Compile-time complexity

SLT lowering walks the reachable expression DAG once to compute fanout.  It
memoizes owned dynamic cost, the width-independent region-slice lower bound,
div/rem presence, purity, and the presence of non-trivial shared descendants
per `NodeId`.  Each mux then examines only its roots and direct operands.  It
does not rebuild a descendant set for every nested priority mux.

Arm lowering uses one cache with an insertion log.  On leaving an arm it rolls
back only entries inserted in that arm; it never clones the global materialized
node cache.  Analysis is therefore linear in reachable DAG nodes and edges,
and cache maintenance is linear in newly lowered arm nodes, independent of a
large unrelated global cache.  Depth-chain and unrelated-cache regression
tests enforce those bounds; there is no depth or node-count cutoff.

## Acceptance gates

Correctness is checked by SIR verification before and after optimization and by
focused tests for shared definitions, aliasing loads, merge parameters,
dominating live-ins, four-state mode, and rejected break-even candidates.
Local same-build `avg_comb_us` A/B measurements are diagnostics only. A
compile-only reduction, a smaller SIR/MIR count, or a partial runtime window is
not an acceptance result. The sole performance gate is the pinned same-input
full Heliodor Linux-boot run implemented by
`scripts/run-heliodor-bench.sh gate`: both `veryl-cc` and Celox must report
`status=pass`, both must report the exact `cy=9ae070 x3=aa pass=1`
architectural marker, Celox must report `compile_only=false`, and Celox wall
time must not exceed the corresponding Veryl wall time. The Celox runner is
built once with the locked release/LTO profile for this final gate; iterative
development runs use `heliodor-dev` without LTO.

The fixed gate on clean Celox commit `e917489e` passed both semantic checks and
the exact `cy=9ae070 x3=aa pass=1` markers. Veryl-CC took `68.409 s` and Celox
took `178.223 s`; the resulting `2.605x` gap failed only the performance
condition. Branch-aware placement is therefore retained as tested compiler
infrastructure, but it has not closed the end-to-end throughput target.

## Current boundary

Binary reverse if-conversion remains only the leaf mechanism. The production
pipeline now includes whole-unit, occurrence- and token-aware DAG
placement. Source provenance and the occurrence action skeleton are verified
first; token SSA then creates versioned `InstValue`s, execution-safety analysis
limits their legal domains, and ScheduleEarly/ScheduleLate derives
state-specific use envelopes. Bottom-up gate/decision selection is followed by
one final `PlacementPlan`, which assigns each pure `InstValue` once to its
latest legal dominating control site. This is the gated-SSA/lazy-code-motion
relationship: profitability selects control regions while verified placement
owns shared pure work. Raw `NodeId` reachability or nearest-common-dominator
placement is not the final value identity. The design uses no mux, block,
iteration, or traversal cap.

The retained implementation recovers complete residual priority regions,
moves legal cross-block value and exact-state-load occurrences, and places
connected pure DAGs in their latest existing control region. A grouped
multi-output fusion trial also passed correctness testing, but extended live
ranges and slowed two complete Heliodor runs, so it was reverted. The remaining
work is target-cost selection for broader multiway `DecisionRegion` forms,
including value tables, jump tables, balanced comparison trees, ordered
wildcard chains, and small branchless tails. Detailed retained/rejected results
are in [Native throughput execution plan](./native-throughput-execution-plan.md).
