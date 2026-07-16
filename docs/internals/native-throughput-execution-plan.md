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

## Related design records

- [Simulator architecture](./architecture.md)
- [Native register allocation](./native-register-allocation.md)
- [Branch-aware mux lowering](./branch-aware-mux-lowering.md)
- [JIT roadmap](./jit-roadmap.md)
- [Heliodor macro benchmark](../benchmarks/heliodor.md)
