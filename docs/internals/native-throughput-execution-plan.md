# Native throughput execution plan

> **Status:** active implementation plan for `perf/native-simulation-throughput`.
> Every implementation step is stopped at its own correctness and Linux-boot
> gate before the next step begins. A smaller IR or a compile-only result is
> not an acceptance result.

This plan closes the native execution-time gap to `veryl-cc` without moving
HDL semantics into instruction selection or replacing the native backend with
an external C/C++ compiler. It separates ordinary compiler infrastructure from
the parts that must understand RTL state and simulation phases.

The starting point is commit `6c3bca60`. With the non-LTO `heliodor-dev`
profile, the pinned Heliodor single-hart Linux workload completed with
`reboot: Power down`, `cy=9ab960`, `x3=aa`, and `pass=1`. The measured full
Celox process took about 233 seconds. These values record the starting point;
only a new full successful run may replace them.

## Non-negotiable rules

1. Do not use LTO for iterative builds or the final measurements in this plan.
   Use the `heliodor-dev` Cargo profile. The fixed `gate` command currently
   forces the release/LTO profile, so it is not used unless that policy is
   changed separately.
2. Do not call a compile-only result, a cycle window, an instruction count, or
   a process exit code a successful runtime result. A Heliodor run succeeds
   only when its normal full-test semantic markers are present.
3. Do not stack a new implementation step on a failed gate. Diagnose or revert
   the current step first.
4. Keep instrumentation out of the generated comb/FF functions. If a diagnostic
   change affects generated RTL code, first reconfirm that the original failure
   still occurs with that exact generated code before relating observations to
   the failure.
5. Preserve four-state behavior, event ordering, observers, simultaneous-domain
   eval/apply semantics, and cascade-clock behavior. A two-state optimization
   must be rejected explicitly in four-state mode unless its X/Z semantics are
   proved.
6. Do not add function-size, block-count, mux-count, iteration, or traversal
   caps as correctness or termination mechanisms.
7. Use one focused commit per completed step. Record the exact tests and Linux
   result in this document before starting the next step.

## Compiler/HDL boundary

The intended pipeline is:

```text
RTL/SLT
  -> arbitrary-width SIR with explicit control and state effects
  -> shared CFG, dominance, loop, and StateSSA analyses
  -> state promotion and decision-region placement
  -> i32/i64 native MIR with target constraints and reload recipes
  -> pressure-aware scheduling
  -> SSA live-range splitting, spill planning, and coloring
  -> x86-64
```

The following are ordinary compiler responsibilities and must not be solved by
HDL-specific thresholds:

- reachable CFG construction, reverse postorder, dominators, post-dominators,
  dominance frontiers, loop/SCC regions, and control dependence;
- pruned SSA construction, mem2reg, MemorySSA def/use/phi placement, GVN, DSE,
  and dominance-safe code motion;
- i32/i64 machine-operation widths and fixed-register/clobber constraints;
- pressure-aware list scheduling, live-range splitting, rematerialization,
  spill placement, coalescing, coloring, and SSA destruction.

The HDL-specific inputs to those mechanisms are:

- stable, working, and committed state versions across comb, FF eval, and FF
  apply phases;
- bit-range aliasing and partial writes to arbitrary-width state;
- the ability to use a still-valid RTL state version as a reload home;
- four-state value/mask behavior;
- recovery of software control from mux/predicate dataflow while preserving
  events and state effects; and
- clock-domain, observer, and cascade-settle boundaries.

A large fused function is not itself a reason to split evaluation units. The
backend must split live ranges and place reloads in a large CFG. Function
partitioning remains an independent code-cache or parallel-execution decision.

## Test protocol used after every step

Every implementation step runs all of the following before it is committed:

```bash
cargo fmt --all -- --check
cargo check -p celox
cargo test -p celox --profile heliodor-dev --lib
cargo test -p celox --profile heliodor-dev --test native_testbench
cargo test -p celox --profile heliodor-dev --test counter
```

Focused tests for the changed component run before this common set. Changes to
CFG, SIR semantics, ISel, MIR, scheduling, register allocation, or native
emission additionally run the same full non-LTO Linux workload:

```bash
HELIODOR_RUNNERS=celox \
HELIODOR_TESTS=test_soc_linux_boot \
HELIODOR_TIMEOUT_SEC=300 \
HELIODOR_CELOX_CARGO_PROFILE=heliodor-dev \
scripts/run-heliodor-bench.sh run
```

Before accepting the run, inspect its full log for exactly one native/O2/
two-state/full-execution configuration, the expected test name, normal kernel
power-down, and the final pass record. The generated Heliodor checkout must be
clean before each run. Wall time is recorded, but correctness is the first gate.

Documentation-only steps run `pnpm exec vitepress build docs` instead of the
Linux workload. Shell fixture changes also run both Heliodor fixture suites.

## Step 0: Freeze the plan and baseline

Deliverables:

- this execution plan and its documentation navigation entry;
- an explicit non-LTO test matrix;
- confirmation that the Heliodor source/testbench checkout is clean; and
- a fresh full starting-point run if the existing executable/log cannot be
  tied unambiguously to `6c3bca60`.

Acceptance:

- VitePress documentation build succeeds;
- repository worktree contains only this step's intended documentation change;
- the baseline log or fresh run contains the complete success markers.

Result:

- `pnpm exec vitepress build docs`: passed;
- Heliodor checkout: pinned commit and clean after removing an obsolete
  untracked `tb/test` debug symlink;
- non-LTO full run: `229.855 s` process time and `229.726 s` runner-reported
  time;
- completion: `reboot: Power down`, `cy=9ab960 x3=aa pass=1`, and
  `CELOX_TEST_RESULT ... status=pass`.

Status: **complete**.

## Step 1: Shared SIR CFG analysis

Implement one reusable, deterministic CFG analysis for SIR instead of keeping
pass-local predecessor, RPO, dominator, and frontier implementations.

Deliverables:

- iterative reachable graph construction with checked block lookup;
- predecessor/successor tables and stable reverse postorder;
- near-linear dominator and post-dominator construction, including multiple
  exits through a synthetic post-dominator root;
- dominance frontiers, post-dominance frontiers, and control-dependence queries;
- natural-loop and irreducible-SCC facts needed by later placement;
- structured errors for malformed or unreachable SIR; and
- migration of global store/load forwarding to the shared dominator/frontier
  result without changing its rewrite policy.

Focused tests:

- linear, diamond, nested-diamond, natural-loop, irreducible-SCC, multiple-exit,
  unreachable-block, deep-chain, and large-wide-CFG fixtures;
- old MemorySSA/mem2reg tests before and after migration;
- per-pass SIR verification.

Acceptance:

- no recursive graph walk proportional to SIR depth;
- analysis work is linear or near-linear in blocks and edges;
- optimized SIR is unchanged for the migration fixtures;
- common tests and full Heliodor Linux boot pass.

Result:

- added deterministic iterative reachable-CFG construction, Lengauer--Tarjan
  dominators, virtual-exit post-dominators, dominance/post-dominance frontiers,
  control dependence, SCC classification, and natural-loop discovery;
- migrated global store/load forwarding from its recursive private CFG and
  iterative dominator implementation to the shared analysis without changing
  its rewrite policy;
- focused tests: 9 shared-CFG fixtures (including a 20,000-block chain, an
  8,192-block wide CFG, and 760 generated differential graphs) and all 11
  global store/load forwarding fixtures passed;
- common tests: 645 library tests, 60 native-testbench tests, and 6 counter
  tests passed; the documented upstream/Veryl cases remained ignored;
- non-LTO full run: `233.042 s` process time and `232.912 s` runner-reported
  time, using the same generated-code semantics and pinned Heliodor revision as
  Step 0; and
- completion: `reboot: Power down`, `cy=9ab960 x3=aa pass=1`, and
  `CELOX_TEST_RESULT ... status=pass`.

Status: **complete**.

## Step 2: Phase-aware StateSSA and mem2reg

Separate the semantic state graph from profitability. Working state that is
non-escaping and definitely defined is analogous to a promotable stack slot:
it is converted to pruned SSA even when the resulting value later needs to be
split or spilled. Stable state remains externally visible, so required stores
remain while dominated loads may use reaching versions.

Deliverables:

- a canonical state fragment identity containing region, absolute address,
  value/mask plane, bit offset, and width;
- explicit MemoryDef, MemoryUse, and MemoryPhi identities for exact fragments;
- phase labels for comb input/output, FF eval working state, and FF apply stable
  state at fused-EU boundaries;
- conservative kills for dynamic, overlapping, eventful, or unknown accesses;
- pruned promotion of eligible working round trips;
- stable-load forwarding that preserves observable stable stores;
- one verified rewrite plan applied atomically after analysis; and
- removal of the duplicate pass-local CFG implementation.

Focused tests:

- branch/loop phis, partial overlap, dynamic aliases, old-stable reads after a
  working write, multi-domain eval-before-apply ordering, observer/event
  barriers, and value/mask-plane separation;
- differential native/Cranelift/Wasm tests for two-state and four-state cases.

Acceptance:

- every rewritten load names one dominating reaching state version;
- every removed working store is non-observable and non-escaping;
- common tests and full Heliodor Linux boot pass before any new forwarding mode
  is enabled by default.

Result:

- added a phase-aware StateSSA graph with canonical address/plane/range
  fragments, explicit def/use/kill/phi identities, pruned phi placement, and a
  verifier for reaching-version identity and dominance;
- made dynamic and overlapping accesses path-local kills, kept four-state
  value/mask storage atomic, and applied promotion plans to a clone only after
  complete analysis and SIR verification;
- promoted exact non-escaping working-state round trips, including loop-carried
  and disjoint fragments, while preserving old stable reads and rejecting
  effectful or phase-external state atomically;
- implemented verified stable-state forwarding for the fused FF suffix, but
  deliberately left cross-phase comb-to-FF forwarding staged until Step 3 adds
  allocator-owned reload recipes. Enabling it at this point reduced SIR/MIR
  instruction count but extended cheap state-backed values across the full CFG:
  the N=128 sorter spill frame grew from 20,048 to 39,736 bytes and its suite
  scaling check regressed to 12.37x. With the mode staged, the unchanged sorter
  thresholds passed 7/7; N=32 to N=128 measured 5.04x and N=128 took 5.61 s;
- fixed a latent single-predecessor inliner bug exposed by StateSSA block
  parameters: dominated uses outside the removed block are now rewritten after
  transitive parameter substitutions are flattened. The inliner now uses a
  deterministic incremental worklist rather than rebuilding and sorting the
  full predecessor map after every merge; its focused 20,000-block fixture
  completed together with the downstream-use regression;
- focused tests: 7 StateSSA fixtures and 18 forwarding/promotion fixtures
  passed, including loop phis, partial/dynamic aliases, phase bypass, stable
  ordering, writeback motion, and four-state behavior;
- common and extended tests: `cargo check -p celox`, all 661 library tests, 60
  native-testbench tests, 9 native/Cranelift/Wasm counter tests, the 7 sorter
  scaling tests, and the complete `celox --tests` suite passed; documented
  upstream/Veryl cases remained ignored; and
- non-LTO full run: `232.172 s` process time and `232.006 s` runner-reported
  time, with one native/O2/two-state/full-execution configuration on clean
  Heliodor commit `7ad830fc`;
- completion: `reboot: Power down`, `cy=9ab960 x3=aa pass=1`, and
  `CELOX_TEST_RESULT ... status=pass` in
  `target/heliodor/results/20260715T070929Z_celox_test_soc_linux_boot.log`.

Status: **complete**.

## Step 3: Reload recipes and allocator-owned splitting

Mem2reg exposes values; it does not promise that they remain in physical
registers. Move all pressure decisions into the machine backend and remove the
threshold-based MIR splitting pre-pass.

Execution slices:

1. **3a -- allocator contract and phase baseline (complete).** Keep the
   generated program unchanged, expose timings for CFG normalization, CSSA,
   constraints, next-use production/verification, and allocation, then run all
   focused allocator tests. With cross-phase forwarding staged, the N=128
   sorter took 5.31 s: maximum scheduled pressure was 792, next-use analysis
   took 29.5 ms, and total register allocation took 390 ms. Enabling the
   forwarding rewrite only for diagnosis raised pressure to 2,785; next-use
   production and its independent verifier took 5.14 s and 3.58 s, spill
   planning took 3.31 s, total allocation took 14.27 s, and N=128 took 24.69 s.
   This ties the regression to unsplit state-backed live ranges rather than
   CSSA (47 ms) or CFG normalization (55 ms). All 101 allocator tests passed
   after the timing boundaries were added.
2. **3b -- reload recipes and validity proof (in progress).** Replace the
   overloaded spill descriptor with explicit constant, state-version, pure
   recomputation, and stack recipes. State recipes use physical MIR load shape
   plus a MemorySSA version; an independent verifier rejects every reload not
   dominated by that still-current version.
3. **3c -- allocator-owned split placement (pending).** Select recipes at
   actual pressure points and reconstruct strict SSA at instruction, CFG-edge,
   and loop boundaries. Keep state-backed values out of global next-use maps
   once their live ranges have been split, and run post-split cleanup.
4. **3d -- retire the old split pass and enable forwarding (pending).** Delete
   `mir_opt::split_live_ranges` and its VReg/gap thresholds, enable cross-phase
   stable forwarding, and require unchanged sorter thresholds plus the common
   and Linux gates.

Deliverables:

- a reload-recipe model distinguishing constants, valid state versions, cheap
  pure recomputation, and stack homes;
- validity intervals or explicit materialization points for state-backed
  recipes so a later state version can never be reloaded accidentally;
- spill/reload cost including loop-region execution weight and target operation
  cost rather than only VReg count or instruction distance;
- allocator split points at uses, CFG edges, and loop boundaries;
- safe rematerialization of constants and selected pure recipes near each use;
- post-spill scheduling/peephole cleanup where inserted operations permit it;
- deletion of `mir_opt::split_live_ranges` after equivalent allocator coverage;
  and
- independent verification that each reload is dominated by a valid home or
  recipe.

The existing Braun--Hack W/S planning, SSA reconstruction, pressure proof,
late Perm construction, and chordal coloring remain the correctness framework.
There is no spill/color retry loop.

Focused tests:

- long same-block and cross-block gaps, diamonds, loop-carried values, cold
  arms, fixed-register operations, div/rem clobbers, state-version kills,
  partial state writes, and rematerialization fanout;
- assertions that a split is placed near uses and that no stale state reload is
  emitted.

Acceptance:

- no fixed `VReg count`, instruction-gap, CFG-size, or iteration threshold
  controls correctness;
- allocation and all verifier phases terminate on the Heliodor function;
- common tests and full Heliodor Linux boot pass;
- runtime must not regress before broader StateSSA forwarding is enabled.

Status: **not started**.

## Step 4: Whole-region mux control and placement

Replace leaf-only cleanup decisions with a verified whole-unit placement plan.
Profitability selects decision regions; dominance-aware placement owns shared
pure work and emits each value once.

Deliverables:

- occurrence- and state-token-aware value identities rather than raw `NodeId`
  reachability;
- execution-safety domains derived from state/effect dependencies;
- ScheduleEarly/ScheduleLate placement bounds from dominators and
  post-dominators;
- bottom-up binary gate selection followed by one atomic `PlacementPlan`;
- shared-arm expressions placed once at their latest legal dominating control
  site;
- multiway `DecisionRegion` recognition for same-selector equality/priority
  chains;
- target choice among value table, jump table, comparison tree, ordered chain,
  and branchless tail; and
- explicit rejection of unsupported four-state control conversion.

Focused tests:

- shared descendants at multiple depths, nested predicates, duplicated source
  occurrences with different state versions, aliasing loads, effect barriers,
  merge parameters, priority/default behavior, wildcard overlap, and large
  decoder chains;
- generated-code differential tests against the branchless form.

Acceptance:

- every placed value has one definition dominating all uses in its execution
  domain;
- unselected pure arm work is absent from the executed CFG path;
- no observable operation changes control domain;
- common tests and full Heliodor Linux boot pass;
- retain the change only when full-run wall time improves under identical
  non-LTO conditions.

Status: **not started**.

## Step 5: End-to-end qualification

After all retained steps:

- run the common test set once more;
- run Heliodor shell fixtures;
- run the full pinned Heliodor Linux test at least twice with `heliodor-dev`;
- run a same-input `veryl-cc` comparison without using Celox LTO;
- confirm the same semantic completion and simulated-cycle marker;
- record median full-process wall time without treating it as a statistical
  correctness argument; and
- update the stale status/baseline sections of the native JIT and Heliodor
  documents.

The work is complete only when correctness is preserved and the remaining
speed difference is backed by full successful same-workload runs. If the target
is not yet reached, this document records the measured remaining bottleneck and
the goal remains open.

Status: **not started**.

## Execution record

| Step | Commit | Focused tests | Common tests | Full Linux result | Wall time | Status |
|---|---|---|---|---|---:|---|
| 0 | `8f908ca2` | VitePress build passed | documentation-only step | pass: `cy=9ab960 x3=aa pass=1` | 229.855 s | complete |
| 1 | this step commit | CFG 9/9; forwarding 11/11 | lib 645/645; native 60/60; counter 6/6 | pass: `cy=9ab960 x3=aa pass=1` | 233.042 s | complete |
| 2 | pending | pending | pending | pending | pending | not started |
| 3 | pending | pending | pending | pending | pending | not started |
| 4 | pending | pending | pending | pending | pending | not started |
| 5 | pending | pending | pending | pending | pending | not started |

## Related design records

- [Simulator architecture](./architecture.md)
- [Native register allocation](./native-register-allocation.md)
- [Branch-aware mux lowering](./branch-aware-mux-lowering.md)
- [JIT roadmap](./jit-roadmap.md)
- [Heliodor macro benchmark](../benchmarks/heliodor.md)
