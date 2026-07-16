# Native throughput execution plan

> **Status:** active implementation plan for `perf/native-simulation-throughput`.
> Every implementation step is stopped at its own correctness and Linux-boot
> gate before the next step begins. A smaller IR or a compile-only result is
> not an acceptance result.

This plan closes the native execution-time gap to `veryl-cc` without moving
HDL semantics into instruction selection or replacing the native backend with
an external C/C++ compiler. It separates ordinary compiler infrastructure from
the parts that must understand RTL state and simulation phases.

The starting point is commit `6c3bca60`. Its non-LTO Heliodor runs reported
`reboot: Power down`, `cy=9ab960`, `x3=aa`, and `pass=1`, in about 233 seconds.
Qualification later proved that `cy=9ab960` was not the expected RTL result:
the same source under Veryl-CC and Veryl-Cranelift completed at `cy=9ae070`.
Commit `138f46eb` fixed the native wide-to-narrow ISel error responsible for
that discrepancy. Consequently, every earlier `cy=9ab960` timing in this
document remains useful as implementation history but is invalid as a final
same-workload performance comparison.

## Non-negotiable rules

1. Do not use LTO for iterative builds or per-step measurements. Use the
   `heliodor-dev` Cargo profile while developing. The final fixed acceptance
   comparison is the one deliberate release/LTO build and run.
2. Do not call a compile-only result, a cycle window, an instruction count, or
   a process exit code a successful runtime result. A Heliodor run succeeds
   only when its normal full-test semantic markers are present and its cycle
   marker matches the trusted same-RTL reference. For the pinned Linux image,
   the accepted marker is `cy=9ae070 x3=aa pass=1`.
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
power-down, the exact `cy=9ae070 x3=aa pass=1` marker, and the final pass
record. The generated Heliodor checkout must be clean before each run. Wall
time is recorded, but correctness is the first gate.

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
2. **3b -- reload recipes and validity proof (complete).** Replace the
   overloaded spill descriptor with explicit constant, state-version, pure
   recomputation, and stack recipes. State recipes use physical MIR load shape
   plus a byte-granular MemorySSA version; an independent reconstruction of
   the final MIR rejects every reload whose exact machine load or reaching
   memory version changed.
3. **3c -- allocator-owned split placement (complete).** Select recipes at
   actual pressure points and reconstruct strict SSA at instruction, CFG-edge,
   and loop boundaries. Keep state-backed values out of global next-use maps
   once their live ranges have been split, and run post-split cleanup.
4. **3d -- retire the old split pass and evaluate forwarding (complete).**
   Delete `mir_opt::split_live_ranges` and its VReg/gap thresholds, evaluate
   cross-phase stable forwarding with identical generated-code paths, and
   retain it only if the sorter and full Linux gates improve.

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
- runtime must not regress; a broader StateSSA forwarding mode which fails the
  full-run gate remains disabled.

Result:

- replaced the old spill descriptor decision with explicit constant,
  state-version, pure-machine-operation, and stack recipes. The planner uses a
  linear recipe-shape scan; after it chooses split points, byte-granular
  MemorySSA is built only for the values and exact points/edges it requested;
- gave MemorySSA entry, write, and phi versions structural identities so an
  unrelated tracked byte or inserted reload cannot renumber a valid recipe.
  A sparse/full differential fixture proves the same requested-point recipes
  before and after unrelated and partially overlapping writes;
- reconstructs strict SSA at selected instruction and edge reloads, uses exact
  post-store state homes including matching register/MemorySSA phis, and
  recursively removes dead reload/phi chains. Narrow stores are homes only
  when MIR semantics prove the value is already zero-extended to that machine
  width;
- independently rebuilds sparse MemorySSA over the final MIR and rejects a
  materialized reload if its physical load shape, pure-operation chain, or
  reaching state version differs. Demand-driven proof reduced the forwarding
  diagnostic's Heliodor compile-only time from `218.210 s` with all-use
  MemorySSA to `79.316 s` without changing its final MIR;
- deleted the threshold-based MIR live-range splitter. An unconditional split
  at every state-store home was also evaluated and rejected: it shortened the
  spill frame but increased executed state reloads and total code;
- evaluated cross-phase forwarding with the same source and testbench. It
  reduced some SIM/stack loads, but increased stack stores and the spill frame
  on the Heliodor fused function. The paired full/compile-only runs imply an
  execution portion of about `183.73 s` with forwarding versus `163.53 s`
  without it; total full-run time was `263.045 s` versus `233.427 s`.
  Therefore the mode remains disabled rather than being called an improvement;
- focused allocator tests: 129/129 passed. Common and extended gates passed:
  `cargo fmt --all -- --check`, `cargo check -p celox`, 688/688 library tests,
  60 native-testbench tests, 9 native/Cranelift/Wasm counter tests, and all 7
  sorter scaling tests; documented upstream/Veryl cases remained ignored; and
- final non-LTO full run: `232.008 s` process time and `231.895 s`
  runner-reported time, with one native/O2/two-state/full-execution
  configuration on clean Heliodor commit `7ad830fc`. The log contains
  `reboot: Power down`, `cy=9ab960 x3=aa pass=1`, and the final pass record in
  `target/heliodor/results/20260715T123014Z_celox_test_soc_linux_boot.log`.

Status: **complete**.

## Step 4: Whole-region mux control and placement

Replace leaf-only cleanup decisions with a verified whole-unit placement plan.
Profitability selects decision regions; dominance-aware placement owns shared
pure work and emits each value once.

Execution slices:

1. **4a -- shared analysis migration and reproducible baseline (complete).**
   Migrate guarded-region sinking, control-flow simplification, and mux
   branchification from private predecessor/dominator scans to the shared SIR
   CFG. Index incoming edges once for path facts. Before evaluating placement,
   make identical optimized SIR produce identical native output: stabilize
   memory layout, SIR-to-MIR VReg allocation, wide block-parameter allocation,
   overlapping-load forwarding choices, and allocator edge reconstruction.
2. **4b -- value occurrence and execution-safety model (complete).** Build
   occurrence-aware value identities, state/effect tokens, and legal
   ScheduleEarly/ScheduleLate bounds over the full CFG.
3. **4c1 -- atomic residual priority-spine placement (complete).** Select
   complete contiguous priority spines bottom-up from one whole-unit analysis,
   place shared pure values once at their leaf or lowest common decision, and
   apply all disjoint regions as one preflighted plan.
4. **4c2a -- cross-block occurrence ownership (complete).** Extend a priority
   region's movable closure through dominating blocks, including exact state
   reads whose MemorySSA version remains valid at the decision edge.
5. **4c2b1 -- existing-CFG whole-unit late placement (complete).** Schedule a
   connected value DAG into its latest legal existing control region, using
   the eventual placement of instruction users and predecessor-edge placement
   for block arguments.
6. **4c2b2 -- grouped multi-output regions (evaluated and rejected).** Extend
   region recognition beyond one contiguous Mux result spine to grouped
   outputs, while retaining explicit MemorySSA and effect-domain restrictions.
   The implementation was correct but failed the full-runtime retention gate,
   so it is not present in the retained tree.
7. **4d -- generated-code and full-run evaluation (complete).** Prove that
   untaken pure work is absent from executed paths, run the common and Linux
   gates after each retained 4c slice, and retain placement only if full
   runtime improves.

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

4a result:

- migrated the three remaining control-flow consumers to `SirCfg`; path-fact
  construction now uses indexed predecessor/successor tables instead of
  repeatedly scanning every block for incoming edges;
- Heliodor pre-optimization and post-optimization SIR remained byte-identical
  across the migration. Their SHA-256 values are respectively
  `38190d2c41df1f9b00df308f6249132a8ac511817f9fc03c6b8341714f9a383e`
  and `ab2c1377c5100ad80c4666f27748bac1ae81da9e878ae3f0f3359d0dc4b6f711`;
- removed five sources of randomized native output: equal-alignment memory
  layout order, SIR register and block-parameter VReg order, arbitrary
  selection among overlapping covering loads, and HashMap-ordered allocator
  edge reload reconstruction. Two independent full traces now match byte for
  byte through pre-SIR, post-SIR, optimized MIR, reconstructed MIR, physical
  assignments, and x86 disassembly; the complete MIR SHA-256 is
  `8bc93950da1d92a96f53bf1dbb491a6e28ce6863041c02b08b50243fd915815e`;
- focused tests passed: shared CFG 9/9, control-flow simplification 6/6,
  guarded-region sinking 20/20, branchification 28/28, and register allocation
  129/129. Common gates passed with 692/692 library tests, 60/60 native
  testbench tests, 9/9 native/Cranelift/Wasm counter tests, and 7/7 sorter
  scaling tests; documented upstream/Veryl cases remained ignored;
- the clean pinned Heliodor compile-only run took `45.397 s`. The full non-LTO
  run took `209.742 s` process time and `209.527 s` runner-reported time, with
  exactly one native/O2/two-state/full configuration and the required
  `reboot: Power down`, `cy=9ab960 x3=aa pass=1`, and final pass markers in
  `target/heliodor/results/20260715T191014Z_celox_test_soc_linux_boot.log`.

This slice establishes a reproducible baseline and shared analysis substrate;
it does not claim that whole-region placement is implemented.

4b result:

- added one deterministic `ValueOccurrence` per SIR block parameter or
  instruction definition. Equal expressions and repeated loads remain distinct
  occurrences; uses retain their instruction operand or predecessor-edge
  position instead of collapsing to a source DAG identity;
- extended StateSSA with a placement-only analysis mode which versions every
  exact load, including read-only LiveOnEntry state, while leaving the existing
  forwarding selection unchanged. A state-read occurrence carries its exact
  fragment and reaching MemorySSA version; dynamic or structurally invalid
  loads are pinned;
- retained per-block StateSSA entry/exit versions so a prospective move to a
  block entry or newly split edge is accepted only when it observes the same
  version. Exact, partial, and dynamically aliasing writes close that execution
  domain;
- built a separate effect SSA chain for Store, Commit, runtime/capture events,
  capture-enable operations, and Error terminators. Dominance-frontier phis
  preserve branch/merge ordering, and every observable occurrence retains its
  original control-dependence domain;
- implemented ScheduleEarly/ScheduleLate sinking bounds over the shared
  dominator tree. Phi operands are uses on predecessor edges, values shared by
  both arms stay above the branch, and cyclic-SCC crossings are conservatively
  rejected until a loop-value proof exists. This slice only computes and
  verifies placement facts; it does not yet rewrite a decision region;
- focused tests passed: placement analysis 9/9 and existing StateSSA 7/7,
  covering distinct occurrences, read-only and write-separated state tokens,
  partial alias kills, unchanged and changed edge versions, merge parameters,
  effect phis/control domains, and unversioned dynamic loads. Common gates
  passed with 701/701 library tests, 60/60 native testbench tests, and 9/9
  native/Cranelift/Wasm counter tests; documented upstream/Veryl cases remained
  ignored; and
- the clean pinned Heliodor full non-LTO run took `204.925 s` process time and
  `204.764 s` runner-reported time. The log contains exactly one
  native/O2/two-state/full configuration, `reboot: Power down`,
  `cy=9ab960 x3=aa pass=1`, and final pass result in
  `target/heliodor/results/20260715T193059Z_celox_test_soc_linux_boot.log`.
  The Heliodor checkout remained clean at commit `7ad830fc`.

4c1 result:

- recognized each maximal contiguous residual priority spine as one decision
  region instead of independently branchifying its Mux leaves. Candidates are
  ordered bottom-up by dominator depth and selected from one placement-analysis
  snapshot; overlapping blocks or value occurrences are rejected before any
  mutation;
- computed a terminal-leaf execution mask for every movable pure definition.
  A leaf-only value is delayed to that leaf, a value shared by several leaves
  is emitted once at their lowest common decision, and a value with any use
  outside the region stays in the head. State reads and observable operations
  remain pinned rather than being treated as pure work;
- preflighted block-ID capacity for the complete plan and then emitted each
  selected spine as its full decision/leaf/merge CFG. No partial region is
  applied when preflight fails;
- verified the relevant Heliodor optimized SIR directly. In the 4b form, an
  expensive `CountLeadingZeros` path was evaluated before the final priority
  Mux spine. In the retained form, outer predicates branch first and that work
  exists only below the required fall-through decisions. Surrounding residual
  Muxes outside the contiguous spine remain, which is why 4c2 is still pending;
- confirmed that temporary plan observation did not change generated code,
  removed that observation, and regenerated the complete trace. The resulting
  pre-SIR, post-SIR, and MIR are byte-identical to the unobserved successful
  candidate. Their SHA-256 values are respectively
  `38190d2c41df1f9b00df308f6249132a8ac511817f9fc03c6b8341714f9a383e`,
  `9b4ddb6393497fac3f7b749b494f06f84e42217a83c5a8bb853d8947e2d67f75`,
  and `5599b01c31b0a82905669b98af00c24966cd74bd75d8dd2cad9433ccb2150040`;
- focused branchification tests passed 35/35. Common gates passed with 708/708
  library tests, 60 native-testbench passes, and 9 native/Cranelift/Wasm
  counter passes; the documented upstream/Veryl cases remained ignored; and
- the clean pinned Heliodor full non-LTO run took `201.262 s` process time and
  `201.059 s` runner-reported time, improving on the 4b `204.925 s` process
  result. The log contains exactly one native/O2/two-state/full configuration,
  `reboot: Power down`, `cy=9ab960 x3=aa pass=1`, and the final pass result in
  `target/heliodor/results/20260715T205844Z_celox_test_soc_linux_boot.log`.
  The cleanup trace's byte identity ties this run to the final generated code.

4c2a result:

- changed placement ownership from a target-block instruction index to a
  concrete `(block, instruction)` occurrence. The dependency walk may now
  cross dominating blocks, but `PlacementAnalysis::can_sink_to_edge` must
  prove dominance, unchanged loop execution frequency, and a legal placement
  edge before an occurrence enters the region;
- allowed an exact state-read occurrence to move only when StateSSA reports the
  same fragment and MemorySSA version at the target block's exit. A write to
  the fragment between the read and decision leaves the load at its original
  point; dynamic or unversioned reads remain pinned;
- closed each movable set over all uses before mutation. External instruction,
  terminator, or other-region uses pin the producer and then transitively pin
  its dependencies. Selected occurrences are scheduled with an iterative
  topological worklist; repeated operands form one dependency edge rather than
  an accidental cycle;
- validated every source occurrence and every Mux from the shared snapshot
  before applying any region. Removal uses unique SSA definitions after that
  preflight, so two disjoint regions can safely move different occurrences out
  of the same dominating block without stale instruction indexes;
- verified the Heliodor result directly in optimized SIR. The previously
  residual chain is now ordered as
  `r29125 -> r29136 -> r29153 -> r29158 -> r29163 -> r29168 -> r29172 ->
  r29179 -> r29261`. The four conversion-flag state loads occur only in their
  selected leaves, and `CountLeadingZeros r29278` occurs only after every
  earlier decision falls through;
- the final complete trace has SHA-256
  `38190d2c41df1f9b00df308f6249132a8ac511817f9fc03c6b8341714f9a383e`
  for pre-SIR,
  `1baa8418b648a5216d803dc71c3b8aec82c236e9cdc7d91c193523a1c98a4cbb`
  for post-SIR, and
  `55dd639147e604cfe32e6d93a686dee33f1d6f9064cf77db70b71384a557c2c6`
  for MIR;
- focused branchification tests passed 37/37, including valid and invalid state
  versions and two atomic regions sharing one definition block. Common gates
  passed with 710/710 library tests, 60 native-testbench passes, and 9
  native/Cranelift/Wasm counter passes; documented upstream/Veryl cases
  remained ignored; and
- the clean pinned Heliodor full non-LTO run took `183.531 s` process time and
  `183.409 s` runner-reported time. The 47-line log has exactly one
  native/O2/two-state/full configuration, `reboot: Power down`,
  `cy=9ab960 x3=aa pass=1`, and the final pass result in
  `target/heliodor/results/20260715T212842Z_celox_test_soc_linux_boot.log`.

4c2b1 result:

- added a whole-unit reverse-topological ScheduleLate computation over the
  existing CFG. An instruction use is anchored at the already-computed target
  of its user, while branch conditions and block arguments remain anchored at
  their branch block and predecessor edge respectively. This lets complete
  producer chains follow a root into one existing arm instead of leaving the
  producers in the common head;
- accepted only targets which are legal under dominance and execution
  frequency and which do not post-dominate the source. Pure occurrences may
  move; a state read may move only when its exact StateSSA fragment and
  MemorySSA version are unchanged at the target entry. Observable effects,
  parameters, changed state versions, and cyclic targets remain fixed;
- evaluated profitability for each connected movement component rather than
  for an isolated cheap Mux or arithmetic node. The profile-free model charges
  the increase in boundary live-in chunks against the complete work skipped on
  the untaken arm, so a CLZ/shift/Mux chain can move as one unit while a cheap
  two-input bit operation which only expands the boundary is rejected;
- preflighted every concrete source occurrence and target, built all touched
  replacement blocks from the same snapshot, removed definitions by exact
  occurrence, and inserted them at target entries in producer-before-user
  order. No CFG edge, block parameter, register identity, or observable
  operation is rewritten by this slice;
- verified the previously residual Heliodor FPU region directly in optimized
  SIR. Before `Branch(r29303 ? b4447 : b4448)`, neither CLZ arm is evaluated.
  The `r29312` chain is in `b4447`; the `r29343..r29346` and
  `r29368..r29371` chains are in `b4448`, with the latter retained at their
  lowest common existing control block because their uses span its nested
  branch arms;
- two complete traces generated around the uninstrumented Linux run were
  byte-identical for pre-SIR, post-SIR, and MIR. Their SHA-256 values are
  `38190d2c41df1f9b00df308f6249132a8ac511817f9fc03c6b8341714f9a383e`,
  `b9ada97c81febb5f5cafc41639b8403d308afcafe991e214513d3ea51d7b1204`,
  and `5e3380d085935eabc45d198f251b34c65f15da83e83dc4286835206ec3b8d7e4`
  respectively;
- focused branchification tests passed 41/41. Common gates passed with 714/714
  library tests, 60 native-testbench passes, and 9 native/Cranelift/Wasm
  counter passes; documented upstream/Veryl cases remained ignored; and
- the clean pinned Heliodor full non-LTO run took `183.378 s` process time and
  `183.259 s` runner-reported time. Its 47-line log contains exactly one
  native/O2/two-state/full configuration, `reboot: Power down`,
  `cy=9ab960 x3=aa pass=1`, and the final pass result in
  `target/heliodor/results/20260715T215231Z_celox_test_soc_linux_boot.log`.

4c2b2 evaluation:

- implemented maximal structured diamond-chain recognition for repeated
  predicates, including inverted boolean aliases. The trial fused every arm's
  outputs atomically into one final merge and retained Store/effect order;
- allowed a state read to cross the removed intermediate merge only when its
  exact StateSSA fragment and MemorySSA version matched the corresponding
  predecessor edge. Aliasing reads, changed versions, cycles, branch arguments,
  intermediate dependencies, and moved observable effects were rejected;
- included the live-in extension of all final merge parameters in the static
  placement cost and preflighted both disjoint chains and the one safe adjacent
  final-merge/source composition before mutation;
- verified the intended rewrite directly in the complete Heliodor post-SIR.
  Both `amo_b_ze/se` and `amo_old_ze/se` became two-parameter final merges with
  their Stores in original order, and each pair retained only one branch on
  `r28596`;
- focused tests passed 46/46 and the trial common gates passed with 719/719
  library tests, 60 native-testbench passes, and 9 native/Cranelift/Wasm
  counter passes. Two traces around the first full run matched byte for byte;
  their pre-SIR, post-SIR, and MIR SHA-256 values were respectively
  `38190d2c41df1f9b00df308f6249132a8ac511817f9fc03c6b8341714f9a383e`,
  `d1248ebd29a63f767264caa609c65ec1c5fb74bcf32ceb9e65cb8c9e14479aea`,
  and `7e091f1c4e7d50259b2c71a1a09eac025dec39ab23cb5b3358a15a3cce595185`;
- both clean pinned Heliodor runs completed correctly at the identical
  `cy=9ab960 x3=aa pass=1` marker, but took `184.120 s` and `190.775 s` process
  time. The logs are
  `target/heliodor/results/20260715T223006Z_celox_test_soc_linux_boot.log` and
  `target/heliodor/results/20260715T223508Z_celox_test_soc_linux_boot.log`;
  therefore the trial failed the runtime gate against retained 4c2b1 and was
  reverted in full; and
- after the revert, the focused branchification tests passed 41/41 and the
  common gates returned to 714/714 library tests, 60 native-testbench passes,
  and 9 native/Cranelift/Wasm counter passes. The worktree and rebuilt non-LTO
  runner now contain the retained 4c2b1 implementation only.

The failed trial shows that reducing source-level branch count alone is not a
sufficient objective: extending two output values through the combined arms
can worsen the downstream live ranges and native layout. Step 4 therefore ends
at 4c2b1 instead of retaining a structurally cleaner but slower CFG.

Status: **complete (4a--4c2b1 retained; 4c2b2 evaluated and rejected)**.

### Correctness prerequisite discovered before Step 5

The first same-source comparison against Veryl exposed a semantic mismatch
which the testbench's pass bit alone did not detect. Veryl-CC and
Veryl-Cranelift both reached the pinned Linux power-down at `cy=9ae070`, while
Celox reached it at `cy=9ab960`. Disabling SIR optimization passes and changing
the fused comb/FF execution modes did not remove the discrepancy.

Cycle-local comparison found the first architectural divergence after cycle
`0x243ea9`. Celox incorrectly marked and retired a load as poisoned. In the
same FF state, `load_viol_w[19]` was true even though the stored
`i_cdb_is_store` signal was false. A second `eval_comb` retained both values,
which rejected comb non-convergence and scheduler ordering as the cause for
this case.

The complete optimized SIR preserved the correct one-bit types. The error was
introduced while lowering a wide right shift to MIR:

- the wide shift left its raw 64-bit low chunk in the scalar register map;
- the destination SIR register was one bit wide, but no physical `& 1` had
  canonicalized that chunk;
- the generic post-instruction bookkeeping nevertheless recorded
  `known_bits=1`; and
- condition lowering trusted that false fact and eliminated its required
  width mask, allowing bit 1 and higher to make a one-bit condition true.

Commit `138f46eb` makes every scalar result produced by wide binary or unary
lowering physically conform to its declared SIR width before `known_bits` can
be consumed. Its direct SIR regression sets bit 66 while bit 65 is clear,
right-shifts a 128-bit value by 65 into a one-bit register, and uses that value
as a Mux condition. It produced `0xffffffff` before the fix and zero after it.

The focused regression, the earlier SAR-width regression, all 715 non-LTO
library tests, 60 native-testbench tests, and 9 native/Cranelift/Wasm counter
tests passed. With all temporary probes removed and the Heliodor source clean
at `7ad830fc`, the first corrected normal run completed with
`reboot: Power down`, the exact `cy=9ae070 x3=aa pass=1` reference marker, and
the final pass record. The non-LTO process time was `198.235 s`.

This correction invalidates `cy=9ab960` runs as semantic acceptance evidence;
it does not undo the separately tested implementation work in Steps 0--4.
Step 5 therefore measures the retained pipeline again from the corrected
semantic baseline.

## Step 5: End-to-end qualification

After all retained steps:

- run the common test set once more;
- run Heliodor shell fixtures;
- run the full pinned Heliodor Linux test at least twice with `heliodor-dev`;
- run a same-input `veryl-cc` comparison without using Celox LTO;
- after the iterative correctness runs, run the fixed release/LTO comparison
  once from a clean committed checkout;
- confirm the same semantic completion and simulated-cycle marker;
- record each full-process wall time directly without turning repeated runs
  into a statistical correctness argument; and
- update the stale status/baseline sections of the native JIT and Heliodor
  documents.

The work is complete only when correctness is preserved and the remaining
speed difference is backed by full successful same-workload runs. If the target
is not yet reached, this document records the measured remaining bottleneck and
the goal remains open.

Progress:

- correctness repair and direct regression: complete in `138f46eb`;
- common non-LTO test set: complete with 715/715 library tests, 60/60
  native-testbench tests, and 9/9 native/Cranelift/Wasm counter tests;
- Heliodor result and acceptance-gate shell fixtures: complete;
- two clean non-LTO Celox Linux runs: complete at `198.235 s` and `184.652 s`,
  both with `cy=9ae070 x3=aa pass=1`;
- paired non-LTO Veryl-CC run: complete at `76.446 s` with the same marker;
- final fixed release/LTO gate on clean Celox commit `e917489e`: complete.
  Veryl-CC took `68.409 s` and Celox took `178.223 s` process time
  (`178.019 s` runner-reported), and both reached normal power-down with the
  exact `cy=9ae070 x3=aa pass=1` marker. Semantic qualification passed, but
  Celox remained `2.605x` slower, so the gate failed only its no-slower-than-
  Veryl performance condition; and
- status-document updates: complete.

Status: **qualification complete; throughput target remains open**.

### Step 6: Separate compiler latency from generated-code throughput

The end-to-end gate time combined source-to-native compilation with Linux
execution. That made the `2.605x` process ratio unsuitable for deciding whether
a native code-generation change improved the hot simulator. The benchmark now
records `compile_elapsed_ns` and `execute_elapsed_ns` independently for Celox,
and provides a `veryl-cc-sync` runner using Veryl 0.20.2's deterministic
synchronous AOT-C configuration. Every Veryl-CC measurement uses a fresh empty
AOT cache so a shared `.so` hit cannot contaminate compiler latency. The
official asynchronous Veryl CLI remains available for the end-to-end acceptance
gate.

The split point is the call into the already-lowered testbench on both runners.
Consequently the compile interval includes simulator initialization and
testbench lowering as well as frontend, optimization, and native code
generation. This keeps the execution interval directly comparable instead of
charging Veryl-only setup work to its generated code.

The first non-LTO full-workload split measurement completed at the identical
`cy=9ae070 x3=aa pass=1` marker:

- Celox: `40.450 s` compile, `137.675 s` execute;
- Veryl synchronous AOT-C with an empty cache: `58.354 s` compile,
  `54.282 s` execute;
- generated-code execution gap: `2.536x`; Celox/Veryl cold-compile ratio:
  `0.693x`.

Subsequent native runtime steps are retained or rejected using full successful
`execute_elapsed_ns`, while compile-time work uses `compile_elapsed_ns`.
Process time remains a separately reported end-to-end metric. Result-schema,
migration, parser, and fixed-gate fixtures cover the split records before the
next code-generation experiment begins.

### Step 7: Remove definitions made dead by spill reconstruction

The exact native trace exposed an allocator cleanup error independently of the
spill decision itself. A state-backed value could be replaced at every use by
a valid point reload, but reconstruction removed only unused definitions which
it had inserted as reloads. The original load and its pure producer chain then
remained in emitted x86 even when no use named them. In the fused Heliodor MIR,
one concrete dead chain was `load.i16 [sim + 133767] -> shr 7 -> and.w32 7`.

Reconstruction now marks definitions backwards from observable, definitionless
MIR instructions through both instruction and phi operands. It removes every
unmarked pure definition in one graph walk, including original definitions,
materialized recipe steps, and cyclic phi webs. Expected-reload records are
removed only for definitions deliberately erased by this walk, so the
independent recipe verifier still rejects any other missing reload.

Two scheduling trials were separated from this change and rejected:

- merely admitting the previously omitted 32-bit MIR operations to the
  pressure-only scheduler reduced the fused spill frame from 46,192 to 44,552
  bytes, but serialized independent extraction work with the Mux/reduction
  spine and slowed execution from 137.675 s to 146.651 s;
- adding entry/exit dependency depth ahead of pressure restored instruction
  parallelism and, together with dead-definition cleanup, executed in
  136.464 s, but expanded the fused spill frame to 96,736 bytes, the eval/apply
  frame to 57,944 bytes, and the eval-only frame to 129,976 bytes. That is not
  a valid allocator improvement, so none of the scheduling trial remains in
  the retained tree.

The retained DCE-only result keeps the original scheduling and all spill-frame
sizes. Focused reconstruction tests passed 11/11 and exact native MIR tests
passed 6/6. The common non-LTO gates passed with 715/715 library tests, 60
native-testbench tests, and 9 native/Cranelift/Wasm counter tests. The clean
pinned Heliodor run completed at the exact `cy=9ae070 x3=aa pass=1` marker with
40.097 s compile time and 137.349 s execution time, compared with the Step 6
Celox baseline of 40.450 s and 137.675 s respectively. The accepted log is
`target/heliodor/results/20260716T034302Z_celox_test_soc_linux_boot.log`.

Status: **complete; scheduler redesign remains open**.

### Step 8: Bound list scheduling by target register capacity

The rejected Step 7 trials exposed two opposite failures. Pressure-only
selection serialized independent bit extraction with the reduction spine and
lost instruction-level parallelism. Unbounded dependency-depth selection
preloaded far more values than x86-64 can hold and delegated the resulting
live-range explosion to spilling. The retained scheduler now uses the actual
allocatable register count `K = 14` as the switch between those policies.

For each bottom-up ready instruction, the scheduler keeps its immediate
pressure delta and longest dependency depths to the region exit and from the
region entry. While both the current live set and the structurally best
candidate project to at most `K`, it chooses dependency depth to expose
independent work. Once that candidate would cross `K`, it chooses the smallest
pressure delta. This creates a bounded ILP window instead of either serializing
the whole DAG or preloading the whole DAG. The schedulable-opcode match is also
exhaustive now; the 32-bit move, ALU, and immediate forms no longer become
accidental region barriers.

The exact Linux trace has byte-identical pre-optimized, post-optimized, and
native-optimized SIR to Step 7, so the measured change begins in MIR
scheduling. Direct post-RA inspection shows the old fused entry spilling and
immediately reloading a common input before streaming extracted intermediates
to stack. The retained order keeps common inputs and reduction chains in the
bounded register window before moving to another independent chain. Exact
spill frames changed as follows:

| Native function | Step 7 | Capacity-aware schedule |
|---|---:|---:|
| `eval_comb` | 37,776 B | 33,960 B |
| `apply_ff[0]` | 0 B | 0 B |
| `eval_apply_ff[0]` | 8,520 B | 8,560 B |
| `eval_only_ff[0]` | 14,768 B | 13,824 B |
| `eval_comb_apply_ff[0]` | 46,192 B | 42,488 B |

Focused scheduler tests passed 15/15 and exact native MIR tests passed 6/6.
The common non-LTO gates passed with 718/718 library tests, 60 non-ignored
native-testbench tests, and 9 native/Cranelift/Wasm counter tests. Two pinned
Heliodor runs both reached normal power-down at the exact
`cy=9ae070 x3=aa pass=1` marker:

- `target/heliodor/results/20260716T035757Z_celox_test_soc_linux_boot.log`:
  41.181 s compile, 136.602 s execute;
- `target/heliodor/results/20260716T040306Z_celox_test_soc_linux_boot.log`:
  41.000 s compile, 136.868 s execute.

Compared with the Step 7 run, code generation is about 0.9--1.1 s slower while
execution is 0.48--0.75 s faster. The execution change is deliberately treated
as small rather than as closure of the throughput gap. This step retains the
bounded scheduling foundation and reduced spill frames; subsequent work must
still address MemorySSA/mem2reg and spill/reload cost directly.

Status: **complete; generated-code throughput target remains open**.

### Step 9: Give native load GVN structural MemorySSA versions

The exact Step 8 MIR contained physical `SimState` loads which reached the
same memory definition but were emitted again in dominated blocks. The old
global GVN could not safely remove all of them: it invalidated load keys while
walking one dominator-tree path and restored those keys when leaving that
subtree. At a CFG join, the join block is a dominator-tree sibling of the
writing arm, so restoration could make a pre-branch load visible again even
when one incoming path had written the same bytes. A regression constructed
with an entry load, one writing diamond arm, and a join load reproduced that
incorrect reuse before this step.

Native GVN now builds sparse structural MemorySSA before rewriting MIR. Each
exact load is keyed by the reaching versions of its bytes plus an unknown-base
and unknown-all version. Writes receive deterministic definitions, iterated
dominance frontiers place memory phis, and a dominator-tree rename gives joins
and loops distinct structural versions. If CFG analysis fails, load CSE is
disabled rather than falling back to path-local invalidation; non-memory GVN
continues independently.

The optimizer and reload analysis now share one physical MIR write-effect
model. Plain stores and `MemCopy` have exact destination ranges.
`SparseCommit` names its destination and dirty/summary bitmap ranges, while
`SparseMarkActive` names its count, flag, and active-list ranges. Pointer
writes, `SparseCommitWorklist`, and `StoreIndexed` remain conservative. Thus a
sparse metadata update no longer destroys unrelated RTL-state reload recipes,
but an indexed state store still invalidates its entire base until MIR carries
or proves an index range.

For a `SimState` load with the same structural version, GVN may reuse a
dominating value even when doing so extends its MIR live range. The allocator's
version-checked state reload recipe can reconstruct that value at uses, so the
choice removes repeated physical state loads without requiring the register to
remain resident. Arithmetic expressions and stack-frame loads retain the old
live-range profitability rule.

The exact optimized SIR is byte-identical to Step 8. In the full exact MIR,
later loads at concrete addresses including `sim+136384..136456`,
`sim+193648..193656`, and `sim+33908889..33908891` were replaced with uses of
their dominating same-version values. MIR trace size fell to 190,410,346 bytes
and post-allocation frames remained bounded:

| Native function | Step 8 | Structural MemorySSA |
|---|---:|---:|
| `eval_comb` | 33,960 B | 34,016 B |
| `apply_ff[0]` | 0 B | 0 B |
| `eval_apply_ff[0]` | 8,560 B | 8,536 B |
| `eval_only_ff[0]` | 13,824 B | 13,792 B |
| `eval_comb_apply_ff[0]` | 42,488 B | 42,448 B |

The selector at `sim+34006414` is still loaded in four successive case blocks
because taken arms contain `StoreIndexed`, whose current effect is the whole
`SimState` base. This step does not claim that indexed alias problem is solved.

Host execution time drifted enough that candidate-only runs ranged from
136.483 s to 149.993 s. Acceptance therefore used an interleaved Step 8 /
candidate / Step 8 A--B--A measurement with separately reported compile and
execute intervals. All three runs powered down with the identical
`cy=9ae070 x3=aa pass=1` result:

- Step 8 before: 40.186 s compile, 146.367 s execute
  (`target/heliodor/results/20260716T044847Z_step8_baseline_ab.log`);
- structural MemorySSA: 39.483 s compile, 137.843 s execute
  (`target/heliodor/results/20260716T045202Z_memoryssa_candidate_ab.log`);
- Step 8 after: 39.326 s compile, 146.216 s execute
  (`target/heliodor/results/20260716T045515Z_step8_baseline_ab2.log`).

The candidate reduced generated-code execution time by 8.448 s, or 5.78%,
against the mean of the two adjacent Step 8 runs. Compile time did not regress,
but it remains a separate code-generation metric and is not included in that
5.78% result.

Focused write-effect, reload-MemorySSA, and MIR-optimization tests passed
2/2, 23/23, and 50/50. The common non-LTO gates passed with 725/725 library
tests, 6/6 exact native MIR tests, 60 non-ignored native-testbench tests, and 9
native/Cranelift/Wasm counter tests; `cargo fmt --check` and `cargo check -p
celox` also passed.

Status: **complete; indexed-memory alias ranges and broader mem2reg remain
open**.

### Step 10: Bound register-indexed state-write effects

Step 9 still treated every `StoreIndexed [sim + constant + vreg]` as a write to
the whole `SimState` base. That was not a property of the RTL or of the index
VReg. ISel already knows the physical destination object and its allocation;
only that fact was lost when the dynamic address became MIR. In one concrete
Linux chain, SIR block `b8172` stores a 32-bit element of a 1,024-element
variable, while `b8174` reloads the two-bit case selector at
`sim+34006414`. The generated indexed writes can reach only the value plane
`[67812912..67817008)`, dirty bitmap `[68375160..68375224)`, and summary bitmap
`[68375224..68375232)`. None can change the selector byte.

`StoreIndexed` now carries an optional closed-open `MemoryAliasRange`. This is
memory-operation metadata describing the physical state bytes whose values may
change; it is deliberately not a width or value range attached to a VReg.
Machine VRegs retain only the target-relevant 32/64-bit opcode semantics. A
bitfield read-modify-write may use a wider machine access while preserving
bytes outside the range, so the metadata describes the semantic memory effect,
not every byte fetched by that access.

ISel supplies the complete destination-plane range for aligned and unaligned
dynamic value/mask stores and dynamic commits. Sparse first-write data stores
use the actual u64-rounded allocation extent; dirty and summary indexed writes
use their exact bitmap extents. The shared MIR write-effect model exposes these
ranges to both structural load GVN and allocator reload MemorySSA. A missing
range remains a safe whole-base clobber for manually constructed or genuinely
unknown MIR. Constant-index folding produces a plain exact-address `Store`, and
native emission is otherwise unchanged. Pointer stores and
`SparseCommitWorklist` remain unknown effects.

The optimized SIR before and after this step is byte-identical. In the complete
native trace, exact loads of the selector at `sim+34006414` fell from 56 to 32;
every register-indexed store emitted by ISel had a bounded range. The resulting
post-allocation frames changed as follows:

| Native function | Structural MemorySSA | Indexed write ranges |
|---|---:|---:|
| `eval_comb` | 34,016 B | 32,176 B |
| `apply_ff[0]` | 0 B | 0 B |
| `eval_apply_ff[0]` | 8,536 B | 7,072 B |
| `eval_only_ff[0]` | 13,792 B | 12,848 B |
| `eval_comb_apply_ff[0]` | 42,448 B | 39,208 B |

Code generation and generated-code execution were measured independently on
CPU 0. All three A--B--A runs reached normal power-down at the identical
`cy=9ae070 x3=aa pass=1` marker:

- Step 9 before: 59.225 s compile, 144.622 s execute
  (`target/heliodor/results/20260716T055002Z_step9_baseline_cpu0.log`);
- indexed-range candidate: 60.763 s compile, 139.287 s execute
  (`target/heliodor/results/20260716T055336Z_indexed_alias_complete_cpu0.log`);
- Step 9 after: 59.821 s compile, 138.620 s execute
  (`target/heliodor/results/20260716T055927Z_celox_test_soc_linux_boot.log`).

The candidate is 1.65% faster than the mean of the adjacent executions, but
the two identical Step 9 binaries differ by 6.002 s, which is larger than that
estimated effect. Runtime improvement is therefore **not established** by this
sample. Compile intervals are reported separately and are not used to infer
generated-code throughput. The step is retained for its proved alias scope,
unchanged SIR semantics, exact MIR load reduction, smaller spill frames, and
successful full-workload result; broader mem2reg and allocator work remains
open.

Focused dynamic scalar/wide ISel, write-effect, reload-MemorySSA, and GVN tests
cover the metadata and its overlapping/non-overlapping behavior. The common
non-LTO gates passed with 729/729 library tests, 6/6 exact native MIR tests, 60
non-ignored native-testbench tests, and 9 native/Cranelift/Wasm counter tests;
`cargo fmt --check`, `cargo check -p celox`, and strict library clippy also
passed.

Status: **complete structurally; runtime effect unconfirmed; throughput target
remains open**.

### Step 11: Make native pseudo scratch registers explicit before allocation

The x86 emitter previously implemented every `SparseMarkActive` with an
unconditional `push rax` / `pop rax` pair around the active-list update. That
temporary did not exist in MIR, so liveness and register allocation could not
choose a dead register or account for the clobber. The complete Step 10 trace
contains 23,350 post-allocation sparse marks. They therefore contributed
23,350 pushes and 23,350 pops to the emitted functions.

Treating RAX as an ordinary instruction clobber was tested and rejected before
runtime qualification. The fixed-register legalization boundary split one
block at every sparse mark: post-allocation block count rose from 63,859 to
87,209, and the two affected functions grew by a combined 37,787 bytes despite
removing the saves. This is the wrong model for a temporary whose identity is
unconstrained.

ISel now emits a zero-code `Scratch` definition immediately before
`SparseMarkActive`, and the sparse mark carries that VReg as an explicit use.
Its incoming bits have no meaning; the def/use pair reserves one allocatable
machine register across the pseudo. Normal liveness consequently prevents the
register from overlapping any value live through the mark, while the emitter
uses the assigned register directly. No arbitrary RTL width is attached to the
VReg, and there is no hidden post-allocation register use.

The pre-optimized, post-optimized, and native-optimized SIR hashes are
byte-identical to Step 10. Post-allocation block count remains 63,859, all five
spill frames are unchanged, and the disassembly changes are concrete:

- `push rax` falls from 23,676 to 326 and `pop rax` from 23,632 to 282, exactly
  removing the 46,700 sparse-mark save/restore instructions;
- `eval_apply_ff[0]` ends at `0x0029a23d` instead of `0x0029d4dd`, a reduction
  of 12,960 bytes; and
- `eval_comb_apply_ff[0]` ends at `0x003495f3` instead of `0x0034c81e`, a
  reduction of 12,843 bytes.

For example, a Step 10 sequence preserves a live RAX value with
`push rax`, updates the worklist through RAX, and then executes `pop rax`.
After this step, allocation selects RDX for the worklist update while the live
RAX value remains untouched; neither stack instruction is emitted.

Code generation and generated-code execution were measured as separate
intervals. Dedicated non-LTO compile-only samples were 41.077 s for Step 10
and 42.647 s for this step; they contain zero execution time. Linux execution
used a CPU-0 Step 10 / candidate / Step 10 A--B--A sequence. All three runs
reached normal power-down at the identical `cy=9ae070 x3=aa pass=1` marker:

- Step 10 before: 132.252 s execute
  (`target/heliodor/analysis/step11_runtime_a_step10_cpu0.log`);
- explicit-scratch candidate: 132.954 s execute
  (`target/heliodor/analysis/step11_runtime_b_scratch_cpu0.log`); and
- Step 10 after: 135.434 s execute
  (`target/heliodor/analysis/step11_runtime_c_step10_cpu0.log`).

The candidate is 0.66% faster than the adjacent-baseline mean, but the two
identical baseline executions differ by 3.182 s, over three times the inferred
0.889 s effect. Runtime improvement is therefore **not established**. The step
is retained because it removes a proved emitter/allocator boundary violation
and its generated stack instructions without changing SIR, CFG shape, spill
frames, cycle count, or workload result.

Focused MIR operand, rewrite, memory-effect, reload, allocation, emission, and
sparse-worklist tests passed, including all 134 register-allocation tests. The
common non-LTO gates passed with 730/730 library tests, 6/6 exact native MIR
tests, 60/60 non-ignored native-testbench tests, and 9/9 non-ignored
native/Cranelift/Wasm counter tests; formatting and `cargo check -p celox` also
passed. The CI-equivalent all-target clippy command passed after its unrelated
pre-existing lint was repaired in a separate commit.

Status: **complete structurally; runtime effect unconfirmed; broader
live-range/reload work remains open**.

### Step 12: Version SIR state loads and thread correlated case edges

The remaining Heliodor selector ladders were not an instruction-selection
problem.  SIR repeatedly loaded the same selector around disjoint state
writes, so ordinary GVN did not give the comparisons one SSA input.  The CFG
then contained several independent equality branches rather than one
correlated case, and every taken arm still entered the suffix of tests which
could no longer match.

SIR GVN now keys an exact state load by its `StateFragment`, structural
MemorySSA version, result type, and observable-effect epoch.  Loads at the
same version can therefore cross joins, read-only loops, and disjoint
stores/commits.  An overlapping write creates a new version; trigger/capture
stores, trigger commits, and runtime/capture callbacks advance the observable
epoch.  Dynamic or structurally invalid loads retain the old path-local
memory epoch.

The StateSSA construction is sparse.  Before building the shared CFG, GVN
groups exact loads by fragment and result type and retains only shapes with at
least two occurrences.  An EU with no possible reuse builds no StateSSA at
all.  Selected analysis still scans every store, commit, and dynamic alias and
still validates exact writer types, but it does not allocate unrelated load
slots, narrowing both work and failure scope to loads which GVN can consume.
In the complete Heliodor SIR the resulting verified version safely removes one
additional 32-bit reload of `inst51.var184`, replacing `r71553` with the
same-version dominating `r71340`.

Control-flow simplification now performs edge-sensitive correlated value
analysis over the full `SirCfg`.  Exact selector equalities and generic
boolean facts are intersected at joins; cyclic SCCs, effectful decision
blocks, and unsupported block arguments are rejected.  Each linear
same-selector case spine is indexed once, so a taken edge reaches its final
merge without enumerating paths or walking every suffix.  Pure predicate DAGs
which remain live outside a skipped suffix are rematerialized at their actual
use blocks, preserving SSA while shortening the original live range.
Suffixes with no chain-external definitions are summarized once from tail to
head, avoiding a hidden per-arm suffix scan in rematerialization validation;
the complete 16-test CFS group, including its 4096-case fixture, fell from
about 7.2 s to 0.98 s in the same debug test profile.

The exact optimized Heliodor SIR shows the intended dynamic path.  `b8169`
loads the selector once.  The value-zero arm `b8172`, value-one arm `b8175`,
value-two arm `b8178`, and value-three arm `b8181` now jump directly to
`b8183`; only a not-taken edge reaches the next equality block.  The complete
post-allocation MIR has the corresponding direct arm-to-merge jumps.  It
still reloads the selector at heavy arm exits to feed a later dispatch, which
is a remaining live-range/reload problem rather than unexecuted-arm work.

GVN can expose those facts after the main CFS run.  Re-running full SCCP after
every GVN produced the same final code but needlessly rebuilt its lattice.
FF, eval-only, and the post-vectorization comb position now run a smaller
post-GVN fixed point containing only dominated-Mux cleanup and correlated
threading.  The early comb position retains full CFS, and the final SIR
boundary retains full `GVN -> CFS`.  Before sparse load selection, this
arrangement produced byte-identical pre-SIR, optimized SIR, native-optimized
SIR, and full MIR to the all-full-CFS candidate.

Code generation and generated-code execution were measured separately.  Host
Cargo build time and IR formatting time are excluded from these comparisons,
and compile-only runs report `execute_ns=0`:

- the CPU-0 Step 11 compile-only sample was 62.419 s;
- the final sparse Step 12 compile-only sample was 62.949 s;
- an adjacent Step 11 full run was 62.883 s compile and 142.445 s execute;
- the two pre-sparsification Step 12 full runs were 64.651/66.607 s compile
  and 141.629/137.984 s execute; and
- the final sparse candidate completed in 67.765 s compile and 139.358 s
  execute.

Every full run reached normal power-down at the identical
`cy=9ae070 x3=aa pass=1` marker.  The final compile-only interval is only
0.530 s above the sampled Step 11 interval, so the earlier roughly 4.25%
code-generation regression has been removed.  Execution is consistently in
the faster direction in this sample, but the two identical pre-sparsification
candidate executions differ by 3.646 s, larger than the inferred improvement;
runtime improvement is therefore not established.

Focused tests cover structural versions across diamonds and loops, aliasing
writes, observable barriers, live-out predicate rematerialization, cyclic and
effectful rejection, selected-load writer type validation, and a 4096-case
spine.  The common non-LTO gates passed with 743/743 library tests, 6/6 exact
native MIR tests, 60/60 non-ignored native-testbench tests, and 9/9
non-ignored native/Cranelift/Wasm counter tests.  `cargo check`, formatting,
strict library/test clippy, and both Heliodor shell fixture suites also passed.

Status: **complete structurally; code-generation regression removed; runtime
effect unconfirmed; arm-exit live ranges/reloads remain open**.

### Step 13a: Share identical allocator edge-reload tails

The exact Step 12 post-reconstruction MIR exposed an allocator artifact after
correlated case threading.  Each of `bb8172`, `bb8175`, `bb8178`, and `bb8181`
ended with the same seven-value reload bundle before entering `bb8183`:
four exact `SimState` loads and three stack loads from offsets 192, 200, and
448.  The merge then contained seven reconstruction phis with one resident
input plus four freshly reloaded inputs.  This duplication was introduced by
Braun--Hack edge coupling and SSA reconstruction; it was absent from SIR and
pre-allocation MIR.

Reconstruction now records the complete shape of a reload-only edge bundle:
its logical value, spill home, and exact immediate, stack, or resolved recipe.
Bundles are tail-merged only when they have the same successor and byte-for-byte
equivalent shapes, occupy the complete suffix before an unconditional jump,
and every successor phi can replace all grouped inputs with the corresponding
shared definition.  Resident predecessors are not redirected.  One new block
contains the canonical reload suffix; grouped predecessors jump to it and the
successor phis collapse those predecessor rows to one shared row.  The
post-reconstruction CFG is normalized and verified once before reload proof,
pressure proof, Perm construction, and coloring; spill planning is not retried.

Reload validity remains structural across that CFG rewrite.  Native MemorySSA
write identities now use `(BlockId, per-block SimState-write ordinal,
variable)`, and phi identities use `(BlockId, variable)`, so block reordering
or insertion of a non-writing reload cannot renumber a version.  Iterative SCC
condensation aliases a MemorySSA phi only when every external input to its
complete SCC canonicalizes to one version.  This removes the trivial wrapper
phi created at a shared reload block without treating a genuinely different
state version as equal.

The complete optimized SIR before and after Step 13a is byte-identical.  In the
complete post-reconstruction MIR, all four arm blocks now end in
`jmp bb14491`; `bb14491` contains the single seven-load bundle and jumps to
`bb8183`; and each of the seven merge phis has exactly the resident `bb14416`
and shared `bb14491` inputs.  The four arm-local spill stores remain.  A taken
arm still executes seven reloads, while the resident edge executes none, so
this step removes static code/phi duplication and does **not** claim a dynamic
reload-count reduction.  The full textual MIR is 202,533 bytes smaller than
Step 12 (191,496,890 to 191,294,357 bytes).

Compilation and generated-code execution were measured independently on CPU 0;
host Cargo builds and full-IR formatting were outside both intervals.  The
interleaved Step 12 / Step 13a / Step 12 runs were:

- compile: 69.770 s / 63.295 s / 66.919 s;
- execute: 139.706 s / 137.092 s / 141.808 s; and
- result in every run: normal power-down at `cy=9ae070 x3=aa pass=1`.

Against the mean of the adjacent Step 12 runs, the candidate compile interval
is 5.049 s (7.4%) shorter and its execute interval is 3.665 s (2.6%) shorter.
The final post-test qualification run also passed, but took 70.140 s compile
and 148.424 s execute.  This wider candidate variation prevents a runtime or
compile-time speedup claim.  The next allocator step must reduce reloads on
the actually executed edge by improving cost-aware resident selection and
live-range placement.

Status: **structurally complete; static edge duplication removed; dynamic
reload reduction remains open**.

### Step 13b: Cost join residency with CFG anticipatability

The Step 13a reload bundle was not caused by SIR.  At an ordinary MIR join,
the spill planner first retained values resident on every processed incoming
edge, then filled every remaining register slot from the union of partially
resident values using only nearest-next-use order.  Keeping such a value at
the join forces a coupling reload on each predecessor which does not already
hold it.  The planner did not ask whether that value was used on every
continuation, nor compare those coupling reloads with the cost of ending the
live range and reconstructing the value at its actual use.

Global next-use analysis now also computes an after-phi anticipatability fact.
The dataflow meet is intersection over all outgoing CFG edges: successor phi
destinations are removed, the corresponding source is added on each incoming
edge, ordinary definitions kill, and upward-exposed uses generate.  Thus a
value used on only one branch is live but not anticipated at the branch head.
The worklist has no iteration or CFG-size cap, and an independent verifier
reconstructs phi-edge uses and ordinary MIR transfer functions before checking
the fixed-point equation for every block.

At a genuine multi-predecessor join, each resident candidate is now evaluated
with the target reload/rematerialization and spill costs.  Keeping it pays the
coupling reloads on incoming edges where it is absent.  Dropping it pays any
needed home-creation stores and, when the CFG proves a use on every
continuation, the later reload on each equally weighted incoming path.  Only
candidates with positive avoided cost are retained; competition for a
register is ordered by loop-exit distance and then avoided-cost/live-range-span
density.  A single-predecessor block is not a reconciliation point and inherits
the translated predecessor `W_exit` exactly.  This last rule keeps operations
inside normalized edge blocks instead of moving them back onto a branch edge;
the existing isolated-edge verifier remains unchanged.

The complete pre-optimization, post-optimization, and native-optimized SIR
files are byte-identical to Step 13a.  In the complete post-allocation MIR,
`bb8172`, `bb8175`, `bb8178`, and `bb8181` again jump directly to `bb8183` and
the shared seven-reload `bb14491` block is gone.  The seven unconditional
loads formerly executed before the join have moved to the paths which use
them: four exact `SimState` loads and two stack reloads appear in the later
`bb8185`, `bb8187`, `bb8188`, `bb8190`, and `bb8201` regions, while the final
value is rematerialized as zero.  Each heavy arm also has two stack-home stores
instead of the previous three.  The formerly resident incoming edge now pays
two stack-home stores, so this exact MIR change is not assumed to be a runtime
win without measurement.

Compilation and generated-code execution were measured independently with
trace/full-IR formatting disabled.  Host Cargo builds were outside both
intervals.  A CPU-0 Step 13a / Step 13b / Step 13a run gave:

- compile: 61.226 s / 61.512 s / 60.899 s;
- execute: 137.664 s / 135.005 s / 131.713 s; and
- result in every run: normal power-down at `cy=9ae070 x3=aa pass=1`.

The candidate compile interval is 0.449 s above the adjacent-baseline mean.
Its execution interval is 0.317 s above that mean and lies between the two
baseline samples, whose executions differ by 5.951 s.  An earlier candidate
qualification took 63.831 s compile and 131.116 s execute.  The generated MIR
change is therefore established, but neither code-generation nor execution
speed changed measurably in these samples.

Focused allocator tests, including independent anticipatability equations,
phi-edge semantics, conditional-versus-guaranteed join retention, and
single-predecessor inheritance, pass 141/141.  The common non-LTO gates pass
750/750 library tests, 6/6 native-MIR tests, 60/60 non-ignored native-testbench
tests, and 9/9 non-ignored native/Cranelift/Wasm counter tests.  Formatting,
`cargo check`, strict workspace clippy, both Heliodor shell fixture suites, and
the VitePress documentation build also pass.

Status: **structurally complete; seven unconditional join reloads delayed or
removed; timing effect unconfirmed**.

### Step 14: Elide unchanged StateSSA writeback edges

The next exact fused SIR inspection exposed a mem2reg writeback defect rather
than a register-allocation defect.  Working-state promotion represents an FF
path which does not update a fragment as `MemoryHome::Stable`.  When that path
entered a writeback phi, the old implementation materialized the memory home
as a STABLE load so that it could supply an ordinary register argument.
`sink_phi_writebacks_to_predecessors` then moved the merged STABLE store to
every incoming edge, including the unchanged edge.  The resulting edge code
loaded an exact STABLE fragment and immediately stored the same register back
to that fragment.

StateSSA promotion now records the exact `StateFragment` only for loads which
it creates at a predecessor tail to represent an unchanged STABLE phi input.
When writeback sinking sees that same proven fragment, it omits the edge store
if and only if the store has neither trigger nor capture effects.  The now-dead
synthetic load and its register definition are removed.  This is provenance
from the mem2reg construction, not a textual load/store peephole: an arbitrary
same-address load is insufficient, and trigger/capture writebacks remain even
when their value is unchanged.

The complete pre-optimized and post-optimized SIR files are byte-identical to
Step 13b, so the source optimization pipeline is unchanged.  The complete
native-optimized SIR and both pre-/post-allocation MIRs contain the intended
path-sensitive change.  For example, fused `b8186` previously began with four
identity pairs for `inst41.var275[63:0]`, `var276[0]`, `var280[1:0]`, and
`var296[0]`; those pairs are absent and the real sparse-state and `var264`
writes remain.  The analogous four pairs in `b8195` are also absent while its
actual `var264 = 0` write remains.  The full diff shows the same exact
load-then-writeback pattern removed on other unchanged FF edges without
removing their updated alternatives.

Code generation and generated-code execution were measured separately with
trace formatting disabled and CPU 0 fixed.  Host Cargo builds were outside the
intervals.  The Step 13b / candidate / Step 13b A--B--A result was:

- compile: 64.276 s / 62.684 s / 64.201 s;
- execute: 132.360 s / 127.036 s / 131.116 s; and
- result in every run: normal power-down at `cy=9ae070 x3=aa pass=1`.

A post-test candidate qualification also passed at 68.105 s compile and
129.461 s execute.  Thus both candidate executions are faster than both
adjacent baselines; the means are 128.248 s candidate versus 131.738 s
baseline, a 3.490 s (2.65%) reduction.  A separate trace-free compile-only run
reported 65.406 s with `execute_ns=0`.  Candidate compilation spans
62.684--68.105 s, so no code-generation-time improvement is claimed.

Focused global StateSSA/mem2reg tests pass 19/19, including unchanged-edge
elision and preservation of a trigger-bearing identity writeback.  The common
non-LTO gates pass 751/751 library tests, 6/6 native-MIR tests, 60/60
non-ignored native-testbench tests, and 9/9 non-ignored
native/Cranelift/Wasm counter tests.  Formatting, `cargo check`, strict
workspace clippy, both Heliodor shell fixture suites, and the documentation
build also pass.

Status: **complete; proved unchanged FF writebacks removed; generated-code
execution improved 2.65% in the retained sample**.

## Execution record

| Step | Commit | Focused tests | Common tests | Full Linux result | Wall time | Status |
|---|---|---|---|---|---:|---|
| 0 | `8f908ca2` | VitePress build passed | documentation-only step | pass: `cy=9ab960 x3=aa pass=1` | 229.855 s | complete |
| 1 | `e3dfa119` | CFG 9/9; forwarding 11/11 | lib 645/645; native 60/60; counter 6/6 | pass: `cy=9ab960 x3=aa pass=1` | 233.042 s | complete |
| 2 | `75bf2636` | StateSSA 7/7; promotion 18/18 | lib 661/661; native 60/60; counter 9/9 | pass: `cy=9ab960 x3=aa pass=1` | 232.172 s | complete |
| 3 | `d4cdb0f7` | allocator 129/129; sorter 7/7 | lib 688/688; native 60/60; counter 9/9 | pass: `cy=9ab960 x3=aa pass=1` | 232.008 s | complete |
| 4a | `f213119a` | CFG 9/9; CFS 6/6; sinking 20/20; branchify 28/28; allocator 129/129 | lib 692/692; native 60/60; counter 9/9; sorter 7/7 | pass: `cy=9ab960 x3=aa pass=1` | 209.742 s | complete |
| 4b | `47006336` | placement 9/9; StateSSA 7/7 | lib 701/701; native 60/60; counter 9/9 | pass: `cy=9ab960 x3=aa pass=1` | 204.925 s | complete |
| 4c1 | `6bff0569` | branchify 35/35 | lib 708/708; native 60/60; counter 9/9 | pass: `cy=9ab960 x3=aa pass=1` | 201.262 s | complete |
| 4c2a | `f11ac186` | branchify 37/37 | lib 710/710; native 60/60; counter 9/9 | pass: `cy=9ab960 x3=aa pass=1` | 183.531 s | complete |
| 4c2b1--4d | `8e1ec0b9` | branchify 41/41 | lib 714/714; native 60/60; counter 9/9 | pass: `cy=9ab960 x3=aa pass=1` | 183.378 s | complete |
| 4c2b2 trial | rejected (no commit) | branchify 46/46 | lib 719/719; native 60/60; counter 9/9 | pass twice: `cy=9ab960 x3=aa pass=1` | 184.120 s; 190.775 s | reverted |
| semantic repair | `138f46eb` | wide shift to one-bit Mux regression; prior SAR regression | lib 715/715; native 60/60; counter 9/9 | pass once: `cy=9ae070 x3=aa pass=1` | 198.235 s | complete |
| 5 | `138f46eb`--`e917489e` | repair regression; Heliodor result/gate fixtures | lib 715/715; native 60/60; counter 9/9 | non-LTO Celox twice and final release/LTO pair passed at `cy=9ae070 x3=aa pass=1` | non-LTO: Veryl 76.446 s, Celox 184.652 s; release/LTO gate: Veryl 68.409 s, Celox 178.223 s | qualification complete; performance failed (2.605x) |
| 6 | split timing | result/gate fixtures passed | lib 715/715; native 60/60; counter 9/9; docs build passed | non-LTO Celox and cold synchronous Veryl AOT-C passed at `cy=9ae070 x3=aa pass=1` | compile: Celox 40.450 s, Veryl 58.354 s; execute: Celox 137.675 s, Veryl 54.282 s | generated-code gap isolated at 2.536x |
| 7 | post-reconstruction DCE | reconstruction 11/11; native MIR 6/6 | lib 715/715; native 60/60; counter 9/9 | pass: `cy=9ae070 x3=aa pass=1` | compile 40.097 s; execute 137.349 s | complete; scheduling trials rejected |
| 8 | target-capacity-aware scheduling | scheduler 15/15; native MIR 6/6 | lib 718/718; native 60/60; counter 9/9 | pass twice: `cy=9ae070 x3=aa pass=1` | compile 41.181 s / 41.000 s; execute 136.602 s / 136.868 s | complete; bounded ILP retained, throughput target open |
| 9 | structural native MemorySSA and same-version state-load GVN | effects 2/2; reload 23/23; MIR optimization 50/50; native MIR 6/6 | lib 725/725; native 60/60; counter 9/9 | A--B--A all pass: `cy=9ae070 x3=aa pass=1` | Step 8 execute 146.367 s / 146.216 s; candidate 137.843 s; candidate compile 39.483 s | complete; execute -5.78%, indexed alias range open |
| 10 | bounded register-indexed state-write effects | dynamic scalar/wide ISel; effects 3/3; reload 24/24; MIR optimization 51/51; native MIR 6/6 | lib 729/729; native 60/60; counter 9/9 | CPU-0 A--B--A all pass: `cy=9ae070 x3=aa pass=1` | baseline execute 144.622 s / 138.620 s; candidate 139.287 s; compile reported separately | structural result complete; runtime effect unconfirmed |
| 11 | explicit allocation of pseudo scratch registers | MIR operand/rewrite; sparse effects/reload/emission; allocator 134/134; native MIR 6/6 | lib 730/730; native 60/60; counter 9/9 | CPU-0 A--B--A all pass: `cy=9ae070 x3=aa pass=1` | compile-only: baseline 41.077 s, candidate 42.647 s; execute: baseline 132.252 s / 135.434 s, candidate 132.954 s | hidden stack operations removed; runtime effect unconfirmed |
| 12 | sparse SIR StateSSA GVN and correlated case-edge threading | StateSSA 8/8; GVN 17/17; CFS 16/16; native MIR 6/6 | lib 743/743; native 60/60; counter 9/9; strict clippy and Heliodor fixtures | all full runs pass: `cy=9ae070 x3=aa pass=1` | final compile-only 62.949 s; final execute 139.358 s; Step 11 adjacent compile-only 62.419 s / execute 142.445 s | structural result complete; compile regression removed; runtime effect unconfirmed |
| 13a | shared reconstruction edge-reload tails | allocator 137/137; native MIR 6/6 | lib 746/746; native 60/60; counter 9/9; strict clippy, Heliodor fixtures, docs | CPU-0 A--B--A and final qualification pass: `cy=9ae070 x3=aa pass=1` | A--B--A compile 69.770 / 63.295 / 66.919 s, execute 139.706 / 137.092 / 141.808 s; final candidate 70.140 s compile / 148.424 s execute | static duplication removed; timing effect unconfirmed; executed-edge reload reduction open |
| 13b | cost-aware join residency with CFG anticipatability | allocator 141/141; native MIR 6/6 | lib 750/750; native 60/60; counter 9/9; strict clippy, Heliodor fixtures, docs | CPU-0 A--B--A and earlier candidate pass: `cy=9ae070 x3=aa pass=1` | A--B--A compile 61.226 / 61.512 / 60.899 s, execute 137.664 / 135.005 / 131.713 s; earlier candidate 63.831 s compile / 131.116 s execute | seven unconditional join reloads removed or delayed; timing effect unconfirmed |
| 14 | unchanged StateSSA writeback-edge elision | global StateSSA/mem2reg 19/19; native MIR 6/6 | lib 751/751; native 60/60; counter 9/9; strict clippy, Heliodor fixtures, docs | CPU-0 A--B--A and final candidate pass: `cy=9ae070 x3=aa pass=1` | A--B--A compile 64.276 / 62.684 / 64.201 s, execute 132.360 / 127.036 / 131.116 s; final candidate 68.105 s compile / 129.461 s execute | exact identity writebacks removed; candidate mean execute -2.65%; compile effect unconfirmed |

## Related design records

- [Simulator architecture](./architecture.md)
- [Native register allocation](./native-register-allocation.md)
- [Branch-aware mux lowering](./branch-aware-mux-lowering.md)
- [JIT roadmap](./jit-roadmap.md)
- [Heliodor macro benchmark](../benchmarks/heliodor.md)
