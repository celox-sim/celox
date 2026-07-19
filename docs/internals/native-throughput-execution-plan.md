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
8. Measure code generation and execution in the same full-test process but as
   disjoint intervals. `compile_ns` ends after native simulator and initial
   testbench construction; `execute_ns` covers only the already-compiled
   testbench run. Do not infer execution by subtracting separate runs or use
   process time to accept a generated-code throughput change.

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
AOT cache so a shared `.so` hit cannot contaminate compiler latency. The fixed
acceptance gate also uses this synchronous runner; the asynchronous Veryl CLI
remains available only as an additional diagnostic runner.

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

The fixed gate requires positive split intervals and exact full-boot markers
from both runners. Its throughput decision compares only `execute_elapsed_ns`;
it reports `compile_elapsed_ns` separately. A fixture proves that slower compile
time alone does not fail execution throughput, while slower execution fails even
when Celox has the shorter total process time.

The real non-LTO runners were also exercised on CPU 0 with the same Heliodor
`test_alu` source set. Both full tests passed. Veryl reported 3.175876163 s
compile and 21.603 us execute; Celox reported 2.683722835 s compile and 57.211 us
execute. This short test qualifies the split boundary and log plumbing only; its
microsecond execution interval is not used as a throughput decision.

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

### Step 15: Give the allocator point-specific MemorySSA reload costs

The next allocator inspection found that the existing reload analysis had two
different levels of knowledge which were used at different times.  Before
spill planning it classified each VReg once from its defining instruction.
A transient value was therefore costed as a stack spill and reload even when a
later RTL state write had committed that exact value.  After planning,
reconstruction could discover the stronger post-store MemorySSA recipe and
avoid the stack slot, but the W/S and eviction decisions had already been made
with the wrong cost.  A pre-allocation transform which eagerly replaced
distant uses with state loads was rejected: it made constants into memory
loads in its unrestricted form, increased code-generation time in every
restricted form, and did not establish an execution-time improvement.

Reload planning now retains two distinct kinds of cost.  Constants, original
SimState loads, and pure expressions rooted in either still have a global
materialization cost.  Values which may acquire a state home after their
definition are found by a linear dependency closure over exact store operands,
pure MIR definitions, and complete register phis.  Only their actual MIR uses
are queried.  Sparse native MemorySSA proves the byte version at each query and
on relevant phi edges; no `(block, live value)` Cartesian table is built.
The planner uses the exact cost for a known local next use and for a known phi
edge, and falls back to the original stack cost for a cross-block or stale
point whose concrete recipe is not proved.

This planning snapshot cannot authorize code generation.  Once the complete
spill plan is known, reconstruction requests only its selected reload points,
rebuilds MemorySSA independently, materializes the selected machine-width
recipe, then rebuilds it once more over the resulting MIR and compares the
expected and actual versions.  A planning-cost mistake can therefore choose a
poor split but cannot turn a stale state value into generated code.

The supporting representation was tightened at the same boundary:

- direct SimState/StackFrame memory and runtime-owned pointer memory are
  separate effect domains, so an indirect runtime store no longer kills every
  direct state recipe;
- canonical result widths are solved to a def-use fixed point over physical
  MIR operations rather than inferred by instruction order;
- a partial read-modify-write carries per-definition `StateInsertDesc`
  provenance naming the inserted source, bit offset, and width; this is a
  MemorySSA relation, not an arbitrary bit width attached to a VReg;
- 33--63-bit inserted fields are reconstructed with the exact x86-64
  shift/mask sequence; and
- native GVN reuses a same-version state-load leader only when doing so does
  not lengthen that leader's live range.

Focused tests prove a post-store home, rejection after an overlapping write,
matching and non-matching register-phi homes, partial-RMW reconstruction,
separation of indirect memory, and the planner's use of different costs for
two uses of the same SSA value.  A planner fixture also forces the exact
one-load recipe to change the allocator-owned split while the stack-only model
retains the prior deterministic MIN choice.

The complete traced Heliodor pre-optimized, post-optimized, and
native-optimized SIR files are byte-identical to the forwarding-disabled
baseline.  The complete post-allocation MIR shows both the intended gain and
the remaining limitation.  In one eval-comb region a retained value removes a
three-operation `load.i16; shr; and` reconstruction while another operand is
loaded directly from its exact byte state home.  Around `bb1008`, however, two
predecessor state reloads and their reconstruction phi move to two separate
uses in the join.  This removes a stack spill/reload and the phi but executes
the same byte load twice when both uses are reached.  Point costs alone do not
price a cluster of nearby uses or represent an already-valid state home in the
W/S state, so that duplication is not treated as solved.

Code generation and generated-code execution were measured separately on CPU
0 with a prebuilt non-LTO `heliodor-dev` runner and tracing disabled.  Host
Cargo build time was outside both intervals:

- compile-only: 64.747553772 s and `execute_ns=0`;
- full-run compile: 65.313464831 s;
- full-run execute: 128.601612984 s; and
- result: normal power-down at `cy=9ae070 x3=aa pass=1`.

The execution sample lies inside the Step 14 variation, so no runtime speedup
is claimed.  The next allocator substeps are deliberately not another eager
MIR rewrite:

1. represent a MemorySSA-proved state version as an allocator home alternative
   instead of only lowering its numerical reload cost;
2. price all uses in the prospective split interval (including repeated uses
   and loop/edge placement), so retaining one reload across a short use cluster
   competes against rematerializing it repeatedly;
3. let SSA spill placement choose register, stack, or state homes and then
   coalesce compatible split ranges; and
4. preserve the existing independent post-reconstruction MemorySSA verifier,
   with focused tests and the split compile/execute Heliodor gate after every
   substep.

Status: **point-specific costs are structurally working and semantically
qualified; execution effect is unconfirmed; allocator-owned state homes and
use-cluster costing remain open**.

### Step 16: Use a valid state version as the final-use spill home

The first state-home slice is deliberately narrower than general per-use
rematerialization.  When pressure evicts a value, the planner may omit creation
of a persistent stack home only if all of the following are true:

- the next local use is the value's final use over the complete CFG;
- sparse MemorySSA proves a path-specific state recipe at that exact use;
- materializing that recipe is strictly cheaper than creating and reloading a
  stack home; and
- reconstruction independently rebuilds the same physical recipe and the
  post-reconstruction MemorySSA verifier accepts its memory version.

The pending recipe is part of the spill plan rather than an opportunistic
reconstruction choice.  It must be consumed at the exact planned use.  The
stack-home verifier excludes only that annotated point; every ordinary point
and edge reload still requires an all-path stack store.  If the value has any
later use, the optimization is rejected and the existing stack path is kept.
This final-use restriction means the recipe reload cannot create a second
reload of the same split range.  Pricing a multi-use cluster remains a later
allocator problem.

An earlier mixed-home trial was rejected.  It kept the allocator's stack
store, then replaced an individual stack reload with a same-cost state load.
The generated MIR therefore retained the store without reducing the reload
count and made locality worse.  Its separated measurements were 65.083 s for
compile-only, 64.040 s compile plus 132.347 s execute for the full run; a
strictly-cheaper variant measured 64.297 s compile-only and 133.574 s execute.
None of that trial remains in the tree.

The retained MIR has the intended allocator behavior.  In one `eval_comb`
region the baseline kept `v12139` live after
`store.i8 [sim + 33996575], v12139` until a later store and instead emitted a
stack store/reload for `v12233`.  The candidate evicts `v12139` without a
stack store, emits `load.i8 [sim + 33996575]` only at its final use, and keeps
`v12233` resident, removing that stack store and load.  Equivalent changes
occur for the exact homes at `33995064` and `34006548`.  The fused function
also coalesces identical branch reload tails exposed by the changed residency;
the complete post-allocation MIR was inspected with VReg, stack-slot, and
block identities retained.  The spill frames change from 31,760 to 31,728
bytes for `eval_comb` and from 38,712 to 38,696 bytes for
`eval_comb_apply_ff`; the SIR files are byte-identical to Step 15.
After the final source cleanup, all three SIR files and the complete
191,062,398-byte MIR were regenerated with the rebuilt runner and compared
byte for byte with the inspected candidate; all four match.  The final MIR's
SHA-256 is `9a5854a85f8b78723b69d4ea0d11b4f2e516684d9a5741ba6b5f35ec61c37c4f`.

Focused allocator tests pass 149/149.  They include a branch fixture where the
same logical home uses the final state recipe on the non-overwritten arm and a
normal stack spill/reload on the overwritten arm.  Spill-plan verification,
the all-path home verifier, reconstruction, and the independent final-MIR
MemorySSA verifier all accept that fixture.  Common non-LTO gates pass 761/761
library tests, 6/6 native-MIR tests, 60/60 non-ignored native-testbench tests,
and 9/9 non-ignored native/Cranelift/Wasm counter tests, together with
formatting, `cargo check`, strict workspace clippy, and the documentation
build.

Code generation and execution were measured separately on CPU 0 using
prebuilt non-LTO `heliodor-dev` runners, with tracing and Cargo build time
outside both intervals.  The first inspected candidate measured:

- compile-only: 65.239625733 s and `execute_ns=0`;
- full-run compile: 64.343935736 s;
- full-run execute: 128.532642253 s.

The runner rebuilt from the final source measured 65.982896312 s compile-only
with `execute_ns=0`, followed by 66.136450286 s full-run compile and
138.830020431 s execute.  Both full runs reached normal power-down at the exact
`cy=9ae070 x3=aa pass=1` marker.  Since the independently regenerated final
MIR is byte-identical while the two executions differ by 10.297 s, neither a
runtime improvement nor a runtime regression is assigned to this step.  It
establishes the missing state-home representation only for a range which dies
at the reload.  The next allocator step must price complete use clusters,
register occupancy, loop frequency, and edge placement before selecting
state, stack, or retained register residency for a multi-use range.

Status: **structurally complete; final-use state homes remove concrete stack
traffic; runtime effect unconfirmed; multi-use cluster costing remains open**.

### Step 17a: Move straight-line cluster and whole-home costs into the spill plan

Step 15 priced only the next local use when choosing a join-entry resident.
That is insufficient even before loop weighting: all ordinary uses in one MIR
basic block execute whenever the block is entered, and the exact MemorySSA
recipe can differ at each use.  The verified next-use index now exposes the
complete suffix of distinct local use positions without rebuilding def-use
lists.  Join reconciliation sums the concrete materialization costs for that
straight-line cluster while continuing to use anticipatability for uses beyond
the block.  It does not count uses on mutually exclusive successor paths.

This changed the concrete `eval_comb` region around `bb1008`.  Step 16 loaded
state byte `33995435` separately at both join uses.  The cluster-aware join
choice reloads the value on each incoming edge, merges it with a register phi,
and uses that phi at the first join use.  The normal eviction decision still
chooses the cheap state home before the second use, so one later point load
remains.  On the `bb1006` and `bb1009` paths this is still two executed loads;
on `bb1010`, whose predecessor already needed the value, it removes one of
three executed loads.  This is partial interval placement, not a claim that
the complete cluster is retained in one register.

A trial which replaced every normal next-use eviction cost by the sum of all
remaining local uses was rejected before a runtime run.  It assumed that one
resident interval avoids every later reload, although intervening pressure may
split that interval.  The target load still remained, `eval_comb`'s spill frame
grew from 31,736 to 32,768 bytes, the fused frame grew from 38,696 to 40,032
bytes, and full MIR grew to 191,246,400 bytes.  None of that trial remains.
The next interval step must compare use-to-use segments and the alternative
victim occupying the same register rather than summing use counts blindly.

Whole-home selection has also moved out of reconstruction.  The completed W/S
plan now records `recipe_homes` explicitly after every point and edge reload
is known.  For each phi-congruence home it compares the aggregate stack cost
of selected spills, reloads, and implicit incoming `SpillPhi` stores with the
aggregate exact-recipe cost.  A recipe home is selected only when every reload
has a MemorySSA-proved recipe and that complete recipe cost is strictly lower.
Reconstruction materializes this decision and is no longer allowed to infer a
different home kind opportunistically.  An independent spill-plan walk proves
every selected point and edge recipe; the all-path stack-home verifier excludes
only those proved recipe homes, and the existing post-reconstruction MemorySSA
verification remains mandatory.

Focused tests prove the complete local-use slice, a repeated-use join choice,
rejection of a whole home after one selected reload loses its state version,
rejection of an expensive pure recipe when an existing stack reload is
cheaper, and selection of that same recipe when it avoids both a spill and a
reload.  Allocator tests pass 153/153 and native MIR tests pass 6/6.  Common
non-LTO gates pass 765/765 library tests, 60/60 non-ignored native-testbench
tests, and 9/9 non-ignored native/Cranelift/Wasm counter tests, together with
the result/gate fixtures, formatting, workspace check, CI-target strict
clippy, and the documentation build.  Workspace-wide strict clippy separately
reports an unchanged `celox-wasm` `explicit_counter_loop` under Rust 1.97;
that crate is outside the repository's clippy CI command and was not changed
as part of this step.

The complete traced SIR files remain byte-identical to Step 16.  Aggregate
home costing reduces full MIR from the join-only candidate's 191,093,808 bytes
to 190,924,170 bytes, while changing the `eval_comb` and fused spill frames
from 31,736/38,696 bytes to 32,392/39,384 bytes; exact generated code, not
either size alone, is the retention criterion.  The runner rebuilt from the
final source regenerated all three SIR files and the complete MIR.  All four
are byte-identical to the inspected aggregate-home candidate; the final MIR's
SHA-256 is
`9171e68fd6f6e23aaf721f706e01c00ba967d8cdf7e0d3e271ff6b26dde520a9`.

All performance intervals below come from the same trace-free full run on CPU
0 with a prebuilt non-LTO `heliodor-dev` runner.  Cargo build and trace
formatting are outside both intervals:

- join-cluster candidate: 64.705698916 s code generation and 132.737511840 s
  execution;
- planner-owned aggregate home candidate: 66.344171947 s code generation and
  133.368712303 s execution;
- rebuilt final source: 65.618217361 s code generation and 136.727778623 s
  execution; and
- all three runs reached normal power-down at the exact
  `cy=9ae070 x3=aa pass=1` marker.

The 0.631 s execution difference is below the already observed run-to-run
variation of byte-identical Step 16 MIR, so no execution improvement or
regression is assigned.  Code-generation and execution results remain
separate; neither process time nor a compile-only subtraction is used.

The remaining Step 17 work is an explicit use-to-use interval model.  It must
price register occupancy and the displaced interval at each pressure point,
carry loop/edge frequency separately from correctness, and then let register,
stack, or MemorySSA state homes compete for each split range.  The current
join cluster and planner-owned home representation are inputs to that solver,
not its substitute.

Status: **structurally complete and qualified; execution effect unconfirmed;
use-to-use interval selection remains open**.

### Step 17b: Compare aggregate homes with the already-selected mixed plan

The aggregate-home comparison in Step 17a still priced every point reload as
if it came from a stack slot.  That is not the plan reconstruction actually
executes: a final-use reload selected in Step 16 is already supplied by its
exact MemorySSA recipe.  Treating that point as a stack reload can make a
whole-recipe home appear strictly cheaper than the existing mixed plan when
the two real alternatives only tie.

Whole-home selection now uses the materialization cost of an already-selected
`recipe_reloads` point and the normal stack cost everywhere else.  Spill and
edge costs remain unchanged.  The focused fixture has a one-operation exact
recipe at the first reload, a two-cost stack reload at the second point, and a
one-cost spill.  Reconstructing both points from recipes costs one plus three,
which ties that actual mixed baseline and is therefore rejected.  The former
all-stack comparison incorrectly priced the baseline as two plus two plus one
and selected the whole home.

This does **not** revive the mixed-home trial rejected in Step 16.  A trial
which automatically changed ordinary point and edge stack reloads into state
recipes retained their stack stores and changed only the reload source.  Exact
post-allocation MIR and normalized machine-code inspection exposed that some
stack reloads had also been folded into their consumers, while the narrow
state loads required explicit zero-extension.  That trial was removed in
full; only the aggregate comparison against recipes already selected by the
spill plan remains.

The regenerated 58,353,245-byte pre-optimized, 19,582,017-byte post-optimized,
and 20,041,423-byte native-optimized SIR files are byte-identical to Step 17a.
The complete 190,924,170-byte MIR is also byte-identical, with SHA-256
`9171e68fd6f6e23aaf721f706e01c00ba967d8cdf7e0d3e271ff6b26dde520a9`.
Allocator tests pass 154/154, the complete library passes 766/766, native MIR
passes 6/6, non-ignored native testbenches pass 60/60, and the native,
Cranelift, and Wasm counter matrix passes 9/9.

The current source was nevertheless exercised through a full CPU-0 non-LTO
Linux run.  The runner directly reported 66.631243151 s for code generation
and 128.829497024 s for generated-code execution, followed by normal power-down
at the exact `cy=9ae070 x3=aa pass=1` marker.  Because the complete emitted MIR
is byte-identical to Step 17a, no execution-time change is attributed to this
step.  The next step remains the explicit use-to-use interval solver rather
than another isolated reload substitution.

Status: **complete; aggregate cost accounting matches the selected plan;
generated code unchanged; use-to-use interval selection remains open**.

### Step 17c: Select no-home use-to-use segments at pressure points

The first explicit interval slice uses one forced eviction and the next
ordinary use in the same MIR block as its unit.  The spill planner already
knows both endpoints exactly and re-runs pressure selection after every use;
the missing state is whether the evicted value owns a persistent home.  Each
candidate therefore has one of three states:

- resident, occupying one register until the interval endpoint;
- absent with no persistent home, requiring either a new spill or an exact
  recipe at that endpoint; or
- absent with a persistent home, requiring only its normal reload.

At a forced eviction, a value which already has a persistent home is never
changed into an isolated state reload.  For a value without a home, the
planner compares the actual `spill + persistent reload` alternative with the
exact MemorySSA recipe at the next local use.  A non-final interval may choose
the no-home recipe only when its materialization cost is no greater than one
persistent reload and its complete immediate cost is strictly lower than
creating the home.  This dominance condition is independent of how many later
splits occur: every selected recipe is individually no more expensive than the
reload it replaces, and it also postpones or removes the one-time spill.  The
existing final-use rule remains less restrictive because no later interval can
amortize a newly created home.

Reaching the endpoint materializes the planned recipe and returns the value to
the resident state without claiming that a stack home now exists.  A later
pressure point solves the next interval again.  If the state version has gone
stale, that later point creates the stack home while the earlier valid segment
remains profitable.  Eviction cost density must use the materialization the
planner will actually emit; an unselected point recipe may no longer lower a
hypothetical stack victim cost.

This slice is deliberately local.  CFG-edge and loop-carried intervals remain
under the existing W/S coupling and aggregate-home rules until execution
frequency and edge placement can be represented without treating mutually
exclusive paths as jointly executed.  Acceptance requires focused fixtures
for repeated exact segments, a later stale segment, and rejection of a recipe
whose per-use cost exceeds a persistent reload, followed by the common tests,
complete SIR/MIR inspection, and the split-timing Linux gate.

The trial met those local invariants but failed the generated-code and Linux
gate.  Around `sim + 33997909`, it replaced two predecessor loads and a
live-through phi with one exact load at the merged use.  The pre-allocation
MIR was unchanged and this local live range was genuinely shorter.  However,
the resulting VReg order changed the existing row-by-row ordinary-phi
coloring.  At a later wide switch join, every arm then needed four additional
register moves before entering the common block.  A locally cheaper reload
decision had therefore perturbed a much larger edge-copy problem which the
scalar spill cost could not see.

The eviction-cost correction by itself completed Linux boot in
64.068906508 s of code generation and 131.027808444 s of generated-code
execution.  Enabling non-final no-home intervals completed in 68.297044559 s
and 141.170907960 s respectively.  Both reached normal power-down at exactly
`cy=9ae070 x3=aa pass=1`, but both regressed from the Step 17b execution result
of 128.829497024 s.  The complete trial changes to `spill_plan.rs` were
reverted.  A future interval solver must include the downstream coloring and
parallel-copy cost instead of proving profitability from materialization and
stack costs alone.

Status: **rejected and fully reverted; the use-to-use interval solver remains
open**.

### Step 18: Color ordinary live phi bundles jointly

Ordinary phi results at one block entry are simultaneous definitions.  The
former row-by-row greedy coloring could give an early phi a source register
when it had an equally good alternative, leaving a later phi unable to retain
its only source register.  This inserted avoidable copies on every affected
predecessor and was the amplification mechanism exposed by the Step 17c
trial.

The allocator now colors all live ordinary phis in a block as one bundle.
Existing constrained `Perm` matching remains separate and is installed first;
dead ordinary phis receive a verifier-visible color without occupying the live
bundle.  For the remaining rows, an exact subset dynamic program over the
target's 14 allocatable registers maximizes already-colored incoming sources
which remain in the destination register.  Required colors, forbidden colors,
and registers occupied by live `Perm` results are hard constraints.  Target
register order supplies a deterministic lexicographic tie-break.  This is
bounded by the physical register set rather than by an arbitrary MIR-size
threshold.  The focused regression has two phi rows for which greedy coloring
keeps one incoming edge copy while joint matching removes both.

The 58,353,245-byte pre-optimized, 19,582,017-byte post-optimized, and
20,041,423-byte native-optimized SIR files are byte-identical to Step 17b.
Every native function's virtual-register MIR before coloring and its planned
spill/reload instruction body are also byte-identical.  Stack traffic remains
17,472 loads and 13,550 stores.  Only physical assignment, phi destruction,
and emitted x86 change.  Across the complete trace, `xchg` falls from 3,186 to
2,686.  Disassembly lines fall by 1,001 in `eval_comb`, 204 in
`eval_apply_ff`, 22 in `eval_only_ff`, and 964 in `eval_comb_apply_ff`, while
`apply_ff` is unchanged.  In the inspected switch, arms which previously
executed four moves before the join now jump directly to the canonical
register assignment.

The focused color tests pass 5/5, the complete library passes 767/767, native
MIR passes 6/6, non-ignored native testbenches pass 60/60, and the backend
counter matrix passes 9/9.  `cargo check`, the CI-target strict Clippy command,
format checking, and the documentation build also pass.

The CPU-0 non-LTO A--B--A sequence used the same split timing contract and
exact RTL marker for every run.  Candidate code generation took 64.923728165 s
and 65.234924356 s around an isolated Step 17b build taking 64.495442984 s;
no code-generation improvement is claimed.  Generated-code execution took
127.868454843 s, 136.098208142 s, and 128.095218025 s respectively.  The
candidate mean is 127.981836434 s, 8.116371708 s or 5.96% below the
contemporaneous baseline.  Every run reached normal power-down at exactly
`cy=9ae070 x3=aa pass=1`.

Status: **complete; avoidable phi-edge copies removed; the interval solver
remains open**.

### Step 19 trial: Schedule reconstructed state reloads before coloring

Step 17c showed that exact-use rematerialization can expose an x86 load-use
dependency even when it reduces a virtual live range.  A bounded trial moved
only the direct `SimState` load at the base of an independently verified
materialized MemorySSA recipe.  It moved at most four MIR instructions, never
crossed an overlapping or unknown state write, a fixed-register use, a
clobber, another candidate reload, or a point which would raise exact virtual
pressure above the 14 allocatable GPRs.  Every pre-existing non-reload
instruction retained its relative order.  The existing independent recipe
verifier then rebuilt MemorySSA and proved that each load still observed the
selected version.

The complete Heliodor pre-optimized, post-optimized, and native-optimized SIR
files are byte-identical to Step 18.  Every function's pre-allocation MIR is
also byte-identical.  After reconstruction, removing direct `SimState` loads
from both traces leaves byte-identical anchor instruction sequences, and the
multiset of those loads is unchanged.  Thus the trial changed placement and
the downstream physical assignment, not dynamic work or SIR semantics.

That downstream assignment is the problem.  Extending a reload's live range
without exceeding capacity can still occupy a preferred color and lose
coalescing.  In the focused large-pressure fixture, the trial caused an
additional callee-saved GPR to be used and introduced register-to-register
moves.  Four virtual MIR positions are also not a physical x86 latency model.
Capacity safety therefore does not prove that pre-color latency scheduling is
profitable.

The CPU-0 non-LTO A--B--A--B--A sequence kept code generation
(`build_native()` plus initial-testbench compilation) and generated execution
(`run_compiled_testbench()` only) as independent intervals.  Cargo build,
source loading, and IR formatting were outside both.  Candidate code
generation took
66.115843522 s, 64.152130441 s, and 66.008406225 s; isolated Step 18 took
63.881776557 s and 65.501068723 s.  Candidate execution took 131.148613176 s,
144.075147775 s, and 137.769898086 s; Step 18 took 132.484523833 s and
129.762540774 s.  The candidate execution mean is 137.664553012 s versus
131.123532304 s, a 6.541020709 s or 4.99% regression.  Every run reached
normal power-down at exactly `cy=9ae070 x3=aa pass=1`.

The trial was fully reverted.  To isolate the assignment confounder, any
latency-hiding follow-up had to run after physical allocation, where a
physical-liveness proof could move a load without changing coloring.

A follow-up trial then isolated that post-color alternative.  It froze the
complete assignment and moved only a verified direct state load through a
window where its assigned physical register was unused.  It did not cross an
overlapping MemorySSA write, a physical definition/use, a fixed clobber, or a
control barrier.  After every move, the independent assignment verifier and
the independently rebuilt MemorySSA recipe verifier both passed.

This removed the coloring confounder completely.  Relative to Step 18, all
three SIR files, all pre-allocation MIR, every physical assignment, every
non-load post-allocation instruction, and the multiset of direct state loads
were byte-identical.  Only the positions of those loads changed.  Nevertheless,
the CPU-0 non-LTO candidate--baseline--candidate sequence regressed.  Code
generation was 69.308217182 s / 65.765700880 s / 67.862099740 s.  Generated
execution was 132.671878073 s / 129.176485869 s / 137.150349833 s.  The
candidate execution mean was 134.911113953 s, 5.734628084 s or 4.44% above the
contemporaneous baseline; its code-generation mean was also 4.29% higher.
Every run reached normal power-down at exactly `cy=9ae070 x3=aa pass=1`.

The post-color trial was therefore also fully reverted.  This result does not
justify a claim about a particular hardware stall source; it establishes that
moving the same executed reloads by a fixed local distance is not the missing
optimization.  The next interval work must reduce executed reloads or choose a
better persistent home/split, while pricing the resulting physical-copy cost.

Status: **rejected and fully reverted; post-allocation reload scheduling
by fixed local distance is closed; interval/home selection remains open**.

### Step 20 trial: Replace join-cluster cost with first-use cost

The Step 17a join heuristic adds the materialization cost of every guaranteed
ordinary use in the local block when it ranks a value for entry residency.
Those uses are not unconditionally separate reloads: after the first reload a
value remains resident until a real pressure point evicts it.  A focused trial
therefore charged only the first guaranteed use and left all later decisions
to the existing `limit` walk.  Its regression fixture had two values first
used together, one of them reused later without intervening pressure; the
first-use model correctly treated their entry reload costs as equal.

That local premise does not hold for the Heliodor blocks which matter.  The
candidate and an isolated Step 18 runner were rebuilt against the same
Heliodor checkout and working directory.  All three complete SIR files and all
pre-allocation MIR were byte-identical.  Only spill reconstruction, coloring,
and emitted x86 changed.  Candidate x86 was 1,161 bytes shorter in `eval_comb`
and 789 bytes shorter in `eval_comb_apply_ff`; the other three native
functions had identical end addresses.

Exact comparison must use original CFG block IDs, because reconstruction-added
block IDs are not stable when the spill plan changes.  In original
`eval_comb` block `bb1767`, pre-allocation `v35635` has six ordinary uses,
while `v35733` and `v35656` have two each.  On the inspected
`bb1810 -> bb1767` path, Step 18 keeps the representatives for `v35635` and
`v35733` across the join and reloads `v35656` twice in the common block.  The
first-use candidate instead carries `v35656`, reloads `v35733` in the common
block, reloads `v35635` twice, and creates a new `v35635` spill.  It also adds
the edge-side reload needed to carry `v35656`.  The concrete executed path is
therefore two stack loads and one stack store larger.  The common block has 20
incoming arms: deleting duplicated arm-local operations can reduce static
code size while operations moved into or recreated in the common block still
execute after the selected arm.

This identifies the failed assumption rather than merely correlating a timing
change.  A first-use reload does not imply residency through later uses;
intervening pressure can split the value again.  Conversely, summing every use
is not an exact reload count either.  The correct follow-up must replay the
actual block pressure transitions for a candidate entry set, including values
displaced by each reload, later evictions, home creation, and outgoing edge
copies.  Neither first-use cost nor raw use count is a sufficient substitute.

The CPU-0 non-LTO candidate--baseline--candidate sequence kept code generation
and generated execution in separate same-process intervals.  Candidate code
generation took 69.351968639 s and 66.217791125 s; isolated Step 18 took
66.531577001 s.  Candidate generated-code execution took 133.902987154 s and
132.918760734 s, versus 128.487157033 s for Step 18.  The candidate execution
mean is 133.410873944 s, 4.923716911 s or 3.83% slower; its code-generation
mean is separately 1.88% slower.  Every run reached normal power-down at
exactly `cy=9ae070 x3=aa pass=1`.

The complete source trial was reverted.  Its focused result is retained as a
solver requirement, not as an optimization: a no-pressure repeated use may
tie at first use, while a pressured repeated-use region must be evaluated by
the operations the complete local plan would actually emit.

Status: **rejected and fully reverted; regression cause identified in the
original CFG; block-transition join solver remains open**.

### Step 21a: Extract the exact spill-planner block transition

The rejected first-use trial showed that a join-entry score cannot stand in
for the operations produced by the rest of the block.  Before attempting a
new join solver, the existing per-block spill walk was therefore extracted as
`plan_block_transition`.  Given an entry resident set and the already-known
entry spill state, it runs the unchanged phi, use reload, pressure-limit,
clobber, definition, re-eviction, and dead-value transfers and returns only
the point operations plus the block's final resident and spilled sets.  The
main planner calls this transition once and commits its result exactly as it
did before the extraction.

This is analysis infrastructure, not a throughput optimization and not an
explanation of the remaining Celox/Veryl execution gap.  The complete pinned
Heliodor trace was regenerated from the same source and working directory.
All four outputs are byte-for-byte identical to the isolated Step 18 baseline:

- `pre_optimized.sir`: `336d6b7bd66ea0c824293dd69c25fe2c7aa9f862b7a070ab73949db0bf3771d4`;
- `post_optimized.sir`: `54886e73f2879a75bf7158351dc39f6a1980cf18916b86518d0780b13cdc27a8`;
- `native_optimized.sir`: `cf909d3946bb1b70bfa084417414caf4406392b3959f7bf7953eba4e716eeddc`;
  and
- `mir.txt`: `5d7b357232819bd2aad35782eca32859ee23407f4f2bf32d50a23d9ca029df35`.

The complete native register-allocation unit set passes 155/155 and the exact
native-MIR integration set passes 6/6.  A trace-free CPU-0 non-LTO Linux run
also reached normal `reboot: Power down` at exactly
`cy=9ae070 x3=aa pass=1`; its separately reported intervals were 74.179 s for
code generation and 144.439 s for generated-code execution.  Because the
complete generated MIR is identical, this single timing is a correctness gate
and not a performance claim.  No solver is added in this step.  The next step
first has to identify the extra generated work behind the remaining
same-workload execution gap and trace it back to the responsible SIR or MIR
decision; only then can this transition be used to evaluate a proved
allocator cause.

Status: **complete as a behavior-preserving analysis refactor; remaining
throughput cause is not yet established**.

### Step 22: Expose pressure scheduling and remove redundant word32 snapshots

The earlier native trace labelled its first MIR section as being before
register allocation.  That section is also before the allocator's internal
pressure scheduler, so it cannot establish the order seen by liveness and
spilling.  Native tracing now has a separate
`MIR after pressure scheduling, before CSSA and spilling` section.  It is
captured only for an explicitly requested compilation trace; normal
compilation does not stringify this additional MIR.  This correction is
analysis infrastructure and was committed separately as `278f6ecb`.

The new observation point exposed a concrete MIR defect in the PLIC priority
scan.  Its RTL computes one predicate per interrupt source and uses that same
predicate both to reduce `best_pri` and to reduce `best_id`.  Selection had
already represented each predicate with a machine-width `Mov32`, but each
consumer received another `Mov32`.  For example, the old optimized MIR
contained a predicate definition and consumer snapshot of the form
`v186 = mov.w32 v184; v51519 = mov.w32 v186`, followed later by a second
`v51669 = mov.w32 v186`.  Copy propagation handled only full-width `Mov`, so
these consumer-specific snapshots remained real nodes and real emitted moves.

The retained correction treats `Mov32` as a copy only when the defining MIR
instruction structurally proves that its source is already zero-extended to
32 bits.  A `Mov32` from an arbitrary 64-bit source remains a real truncating
definition.  The proof uses only the machine widths represented by MIR; it
does not add arbitrary HDL widths to virtual registers.  The regression test
starts with a 64-bit load, retains the first truncating `Mov32`, and removes
only two later snapshots of that narrowed value.

This is a proved generated-work defect, but not the complete explanation of
the Celox/Veryl gap.  After the redundant copies are removed, the exact
post-scheduler PLIC MIR still completes the priority reduction through
`v334`, then starts the ID reduction at `v366 = select v184, ...`.  Thus the
shared predicates from `v184` onward remain live between the two reductions.
The post-allocation trace stores later predicates to stack slots and reloads
them for the ID reduction.  The next scheduler step must address this shared
frontier without applying the rejected global depth heuristic, which enlarged
the complete Heliodor frames.

All three SIR outputs are byte-identical to Step 21a, so this result is
isolated below SIR:

- pre-optimized SIR:
  `336d6b7bd66ea0c824293dd69c25fe2c7aa9f862b7a070ab73949db0bf3771d4`;
- post-optimized SIR:
  `54886e73f2879a75bf7158351dc39f6a1980cf18916b86518d0780b13cdc27a8`;
  and
- native-optimized SIR:
  `cf909d3946bb1b70bfa084417414caf4406392b3959f7bf7953eba4e716eeddc`.

The exact spill-frame changes for `eval_comb`, `apply_ff`, `eval_apply_ff`,
`eval_only_ff`, and fused `eval_comb_apply_ff` are respectively
32,352 to 30,696 bytes, 0 to 0 bytes, 7,216 to 7,288 bytes, 13,048 to 7,024
bytes, and 39,344 to 37,960 bytes.  The small `eval_apply_ff` increase is
retained rather than hidden; the full workload is the acceptance gate.

The focused width regression passes, all 53 MIR-optimization tests pass, all
155 native allocator tests pass, the complete library passes 768/768, and
native MIR passes 6/6.  A same-process, CPU-0, non-LTO Linux run reached
normal `reboot: Power down` at exactly
`cy=9ae070 x3=aa pass=1`.  Code generation took 70.552166860 s and generated
execution took 123.199303086 s.  The comparable Step 21a run took
74.178878989 s and 144.438914063 s respectively.  This exact pair establishes
that removing the snapshots is useful; it does not assign the whole runtime
change to stack traffic, and it does not explain the remaining Veryl gap.

Status: **one concrete MIR work defect removed and qualified; shared-predicate
scheduling, state traffic, and the remaining generated-execution gap remain
open**.

### Step 23: Recover coupled updates and place dynamic state reads in a loop

The Step 22 PLIC trace exposed a defect above MIR scheduling.  One source
predicate updates both `best_pri` and `best_id`, but optimized SIR represented
those results as two distant Mux recurrences.  The final predicate was also a
flattened `eligible && priority_greater` value.  Consequently, recovering only
the final Mux as control flow still loaded and compared the priority on the
ineligible path.  The reference AOT disassembly instead branches on eligibility
before loading and comparing the priority.

Branchification now recovers correlated same-predicate recurrences as one
conditional tuple update.  For a two-state one-bit `LogicAnd`, it emits the
eligibility guard first and evaluates the second operand only in the true
block.  This is done at SIR, where the scheduler can still place the recovered
dataflow; ISel does not infer source short-circuit semantics.

Two CFG/StateSSA restrictions initially prevented the delayed work from moving
with that control flow:

- placement pinned every occurrence whose origin was in a cyclic SCC, even
  when the destination was a dominated block in the same loop iteration.  It
  now permits ScheduleLate only inside the exact same natural-loop nest of a
  reducible SCC.  Sibling loops, nested loops, SCC boundaries, and irreducible
  SCCs remain closed;
- placement StateSSA previously created versions only for exact static access
  shapes.  A dynamic indexed load therefore had no state version and was
  pinned.  Placement-only StateSSA now gives same-width dynamic loads a
  conservative address-wide version; every static or dynamic store to that
  address kills it.  Forwarding and GVN retain their narrower exact-access
  analysis.

With those changes, the exact optimized PLIC SIR checks `pending && enable`
before the dynamic priority load and `GtU` for 14 of the 15 candidates in each
recovered loop chunk.  MIR preserves that order.  One dynamic priority load is
still unconditional because its candidate is scheduled across the recurrence
boundary, so this step does not claim complete decision-region placement.

The final trace was regenerated from the same source metadata order as Step 22.
Its pre-optimized SIR hash is identical to the baseline, and its complete
outputs are:

- pre-optimized SIR:
  `336d6b7bd66ea0c824293dd69c25fe2c7aa9f862b7a070ab73949db0bf3771d4`;
- post-optimized SIR:
  `babcca2ac53a003eaf77dab35ae45faf052802de614b9a660b4d024eeddf5900`;
- native-optimized SIR:
  `60b82bc32d0a021dd07f68512b6cb1f874775e34b5945a0927834465f7d97fe4`;
  and
- complete native MIR, assignment, and disassembly:
  `8b04a2d75da3c50d4418b91dbba87f079f16810de431c9ad5f23fb1c70f14636`.

The spill frames for `eval_comb`, `apply_ff`, `eval_apply_ff`, `eval_only_ff`,
and fused `eval_comb_apply_ff` are respectively 30,552, 0, 7,288, 7,024, and
37,768 bytes, versus 30,696, 0, 7,288, 7,024, and 37,960 bytes in Step 22.

Focused tests pass 44/44 for BranchifyMux, 12/12 for placement, and 8/8 for
StateSSA.  The common gate passes `cargo fmt --all -- --check`,
`cargo check -p celox`, strict Clippy, 774/774 library tests, 60
native-testbench tests with one documented upstream case ignored, and 9
native/Cranelift/Wasm counter tests with three Veryl cases ignored.

A same-process CPU-0 non-LTO Linux run reached normal `reboot: Power down` at
exactly `cy=9ae070 x3=aa pass=1`.  Code generation took 77.247215690 s and
generated execution took 120.989709386 s.  Step 22 generated execution took
123.199303086 s, so this isolated run is 2.209593700 s, or 1.79%, faster.  The
code-generation interval is reported separately and no compile-time
improvement is claimed.  The measured execution gain proves this defect
matters, but its size also proves that it is not the complete explanation of
the 2.536x Veryl generated-code gap.  Cross-phase state promotion and the
allocator's split/home decisions remain open.

Status: **complete and qualified as one SIR/StateSSA placement improvement;
the dominant generated-execution gap remains unexplained**.

### Step 24: Re-evaluate cross-phase stable forwarding on the current pipeline

The previously staged cross-phase stable-slot rewrite was enabled only for a
diagnostic build of the current Step 23 pipeline.  The source RTL was unchanged:
the pre-optimized SIR hash remained
`336d6b7bd66ea0c824293dd69c25fe2c7aa9f862b7a070ab73949db0bf3771d4`, and
the post-optimized SIR hash remained
`babcca2ac53a003eaf77dab35ae45faf052802de614b9a660b4d024eeddf5900`.
The diagnostic native-optimized SIR and complete MIR hashes were respectively
`380d95cf593dfbceac3b4af564de5b3525e6ab0d8a54773d9e0419bfaf550e48` and
`69e9807199dd8396ba2952f24046429712e2bdaa6a8f282106e69f50bb7f28ac`.

The complete MIR disproves the simple hypothesis that this existing switch
already removes the important comb-to-FF round trips.  For example, the comb
store to SIM offset 33,997,788 remains, and the FF suffix still loads that
offset.  The rewrite changes other FF expressions and introduces additional
reloads on their control-flow paths, but the fused spill frame changes only
from 37,768 to 37,864 bytes.  It therefore does not implement the required
phase-boundary MemorySSA/home selection.

A same-process CPU-0 non-LTO run of that exact diagnostic binary reached normal
`reboot: Power down` at exactly `cy=9ae070 x3=aa pass=1`.  Code generation took
82.192516725 s and generated execution took 121.003125285 s.  Step 23 took
120.989709386 s to execute, so the difference is 0.013415899 s and is not a
throughput improvement.  The switch remains disabled and the worktree contains
no retained diagnostic source change.

Status: **rejected without a commit; the existing cross-phase rewrite is not a
large-gap solution**.

### Step 25: Identify one dominant hot-path difference before further changes

Step 25 deliberately stops searching for isolated Mux or instruction-count
improvements.  It must identify a difference that occurs on a substantial
fraction of generated execution before implementation begins.

1. Rebuild the unchanged Step 23 source with the non-LTO `heliodor-dev`
   profile.  Sample the Linux execution with the existing basic-block perf map;
   do not instrument RTL, SIR, MIR, or emitted machine code.  Retain the exact
   Linux terminal marker and the trace hashes that identify the generated code.
2. Sample the matching Veryl AOT execution and map its hot samples to the 31
   emitted C chunks.  Sampling data is only a locator: inspect the complete
   post-RA instruction paths and corresponding C/RTL before assigning a cause.
3. For the dominant Celox blocks, compare the actual executed transition with
   Veryl.  Separate state-memory round trips, stack spill/reload, parallel-copy
   (`mov`/`xchg`) repair, duplicated comb/FF evaluation, and work skipped by
   control flow.  Do not infer hot cost from whole-function size or cold code.
4. Select an architectural change only if the affected transitions account for
   at least a substantial hot share or occur on the full per-cycle path.  The
   likely design choices are phase-boundary MemorySSA homes visible before
   pressure scheduling, or bounded allocation regions with explicit state
   homes; the profile and exact code, not this prior, decide between them.
5. Test each retained substep separately: focused optimizer/allocator tests,
   format/check and the common native tests, a complete exact SIR/MIR trace,
   then the same-process CPU-0 Linux gate.  Use non-LTO builds while iterating
   and perform a release/LTO qualification only after the complete step wins.

The unchanged Step 23 code was then measured with grouped retired-instruction
and cycle sampling.  Sampling did not change RTL, SIR, MIR, or emitted code,
and both sampled executions reached the exact `cy=9ae070 x3=aa pass=1`
power-down marker.  The uninstrumented Step 23 timing remains the acceptance
baseline; sampled wall times are not used as throughput results.

The generated Celox function retired approximately 1.794 trillion
instructions, while the synchronous Veryl AOT functions retired approximately
0.594 trillion instructions for the same tick count.  Celox therefore executes
about 3.02 times as many generated instructions.  Its approximate generated
IPC is higher, not lower (`2.15` versus `1.63`), so front-end starvation from
the large function is not the dominant explanation.  The uninstrumented
execution times are `120.989709386 s` for Step 23 and `54.994564647 s` for the
synchronous Veryl AOT run, a 2.20x wall-time gap which the higher Celox IPC
partly hides.

Retired-instruction samples were mapped back to the exact Step 23 x86
disassembly and used only to locate code for inspection.  Stack accesses are
about 7.3% of Celox generated instructions and 11.6% of Veryl generated
instructions.  In absolute terms, the excess Celox stack instructions explain
only about 5% of the complete retired-instruction gap.  Bounded allocation
regions may still improve the 37,768-byte frame, but regional register
allocation is not the first dominant fix.

The exact hot SIR and MIR instead expose lost aggregate value information.
For example, the comb prefix writes many disjoint ranges of the 839-bit
`inst51,var6` state object, including `0+:65`, `65+:64`, `742+:32`,
`774+:64`, and `838+:1`.  The FF suffix then reads many of those ranges and
also contains a source-level 839-bit load.  The existing StateSSA identifies a
slot by one exact `(address, offset, width)` access shape.  Every differently
shaped overlapping access is therefore a `Kill`/escape instead of a use or
definition of the same aggregate value.

A read-side scalar-replacement trial proved that the source-level wide load is
not itself the missing optimization.  The existing ISel wide-chunk liveness
and MIR DCE already reduce that 839-bit load to the six containing machine-word
loads needed by its surviving projections.  Replacing the SIR load with three
contiguous projection loads produced the same six loads and the same
shift/or/mask operations in the complete pre-allocation MIR; only virtual
register numbering changed.  The trial was fully reverted without a runtime
measurement because it did not change the retained generated work.

The corresponding Veryl C leaves these accesses visible to a general O2
pipeline long enough for aggregate scalar replacement, store-to-load
forwarding, and DSE.  It can keep selected range values in SSA while retaining
the packed object as a memory home.  Celox instead lowers overlapping access
shapes back to ordered state-memory effects before it has represented their
common range versions.  The problem is therefore not the external packed ABI
or the mere existence of a packed home; it is failure to preserve and use the
optimization freedom available before that home must be observed.

This representation difference agrees with the complete dynamic instruction
comparison.  Celox executes approximately 4.4x as many `and` instructions,
5.7x as many shifts, 16x as many zero-extending loads, and 19x as many
conditional moves.  Celox and Veryl execute roughly the same absolute number
of branches; Celox's lower branch percentage is caused by the additional
scalar packing dataflow, not by a branch-free hot path replacing all Veryl
control flow.  Veryl SIMD instructions are only about 6.6% of its generated
instruction stream, so SIMD alone cannot explain the threefold retired-work
difference either.

Status: **complete; the dominant gap is failure to preserve aggregate range
values across state memory.  Read-side wide-load lowering is already sparse;
range-aware MemorySSA, write-side promotion, placement, and allocator homes
remain missing**.

### Step 26: Preserve aggregate ranges through StateSSA and allocation

Implement the missing aggregate value layer without changing the external
packed layout or RTL storage semantics at observable boundaries.  Logical
ranges are SSA values; native words are only a lowering and memory-home choice,
not the identity or declared width of those values.

1. For each eligible static object, collect the endpoints of every static load
   and store over the complete fused CFG.  Partition only at those semantic
   access boundaries.  Reject an object initially if it has a dynamic/indexed
   access, address alias, commit with unresolved phase semantics, four-state
   storage, trigger, or capture effect.  Do not partition at arbitrary 32- or
   64-bit machine boundaries.
2. Build pruned SSA names for the resulting non-overlapping logical ranges.
   Each access is a use or definition of all ranges it covers; it is no longer
   a kill merely because its width differs from another access.  A partial
   store defines its covered ranges, a load composes only its requested
   ranges, and joins/loops receive range parameters from dominance-frontier
   placement.
3. Preserve the packed state object as a lazy memory home.  Keep an unchanged
   range in memory, materialize an SSA range near a consumer when carrying it
   would create a long live range, and write dirty ranges only where an
   observer, aliasing barrier, phase boundary, or function exit requires the
   packed state.  Coalescing adjacent writebacks into native stores is a late
   lowering decision, not StateSSA identity.
4. Connect range versions to allocation recipes before removing broad sets of
   loads or stores.  The allocator may carry, rematerialize, reload from a
   still-valid state home, or spill a range according to its actual use
   clusters.  It must not turn aggregate promotion into one whole-function
   live range, as the rejected exact-slot cross-phase trial did.
5. Verify the rewritten EU independently: every replaced load is composed from
   dominating range versions, every state-home reload observes the same
   MemorySSA version, every removed dirty store reaches all required
   writebacks, and no dynamic/effectful access was admitted.  Add straight-line
   overlap, diamond, loop, mixed-width, unchanged-edge, phase-boundary, and
   rejection tests before enabling the rewrite in native fused emission.
6. For each retained slice, run its focused tests and the common non-LTO gates,
   regenerate the complete optimized SIR/MIR/assignment/disassembly trace,
   and run the same-process CPU-0 Linux test.  Retain the implementation only
   if the exact tick/result is unchanged and generated execution improves
   substantially.  Use final release/LTO qualification only after the complete
   step wins.
7. Inspect the retained complete MIR and machine code.  If range promotion
   removes the repeated load/extract and partial-write dataflow, proceed to
   allocation-region and use-cluster work for the remaining reloads.  Do not
   tune allocator heuristics against an IR which has already discarded the
   aggregate versions they need.

The first implementation trial deliberately stopped after access-boundary
range SSA and load replacement.  It built pruned range phis over the complete
fused CFG and passed an independent dominance, predecessor-coverage, partition,
and source-width verifier.  In the exact native SIR, fused `b13977` changed
from reloading the 839-bit `inst51,var6` object and its fields to composing the
same value from the reaching comb definitions.  The pre- and post-optimization
source SIR hashes remained unchanged, so this was an isolated native-fusion
experiment.

That trial also proved why load replacement alone is not mem2reg.  It retained
all packed-state stores while making the reaching comb definitions ordinary
SSA operands of the FF suffix.  The allocator therefore had to preserve those
long ranges in registers or stack in addition to writing the existing packed
home.  The fused spill frame grew from 37,768 to 38,632 bytes.  In particular,
an unaligned logical range can span more than one physical state load, while
the current reload recipe represents only one load followed by unary
operations.  Such a range cannot select its packed state as a reload home and
falls back to an additional stack home.

All 780 library tests, 60 non-ignored native-testbench tests, and 9
native/Cranelift/Wasm counter tests passed.  A trace-free CPU-0 Linux run also
reached normal `reboot: Power down` at exactly
`cy=9ae070 x3=aa pass=1`.  Nevertheless, code generation regressed from the
Step 23 77.247215690 s to 124.736860207 s, and generated execution regressed
from 120.989709386 s to 155.679387691 s.  A separate compile-only phase run
located 24.229301002 s in fused SIR merge/optimization and 12.161295906 s in
fused allocation; these phase numbers are locators, not a performance claim
against an unmeasured phase baseline.  The implementation was completely
removed without a commit.

The next slice must therefore combine range SSA with lazy writeback or an
equivalent optional reload/carry representation.  A promoted range may not
keep both an eagerly updated packed home and a newly mandatory stack home.
Unaligned ranges also need a verified multi-load state recipe, or must retain
their load until allocation chooses to carry the value.  CFG construction must
share one sparse range dataflow across fragments rather than running an
independent whole-CFG liveness traversal for every partition.

As a prerequisite, the allocator now records the logical bit range observed by
each packed-state reload home.  A validated physical read-modify-write advances
the MemorySSA version of a pre-existing home when it changes only disjoint
logical bits; an overlapping update still invalidates the home.  Candidate
homes are found through a byte-indexed sparse map, so a physical write does not
scan all live homes.  Focused tests cover both preservation and invalidation.
This is deliberately store-home metadata, not a declaration that a MIR virtual
register has an arbitrary HDL width.

The complete Step 26a trace has byte-identical pre-optimization,
post-optimization, and final native SIR to Step 23.  The pre-allocation and
pressure-scheduled MIR bodies are also byte-identical after virtual-register
renumbering.  Complete normalized post-allocation inspection found four local
changes in both `eval_comb` and the fused function: one 12-bit stack
spill/reload pair became a reload/extract from its still-valid packed home, and
two one-bit reloads moved from separately materialized state bytes to their
containing packed bytes plus shift/mask extraction.  The fused spill frame fell
from 37,768 to 37,760 bytes.  This removes one unnecessary secondary home but
does not yet remove the repeated packed update dataflow.

All 776 library tests, 60 non-ignored native-testbench tests, and 9
native/Cranelift/Wasm counter tests pass.  A trace-free non-LTO CPU-0 run
reached `reboot: Power down` at the unchanged
`cy=9ae070 x3=aa pass=1`.  Code generation took 75.664875519 s and generated
execution took 114.832519883 s, versus the Step 23 77.247215690 s and
120.989709386 s.  This retained prerequisite is therefore non-regressing and
removes concrete stack traffic, but its approximately 5% execution reduction
is not the large aggregate-promotion result required by this step.

Status: **in progress; disjoint bit-range state homes are retained as an
allocator prerequisite.  Forwarding-only range SSA was rejected, and lazy
writeback plus allocation-owned materialization remains the next implementation
boundary**.

### Step 27: Replace split-before-home planning with unified live-range allocation

The second Step 26 trial proved that lazy writeback by itself does not supply
the missing allocation decision.  It represented access-boundary atoms in
range SSA and kept wide store sources as shared carriers, so the first trial's
one-bit extraction/reassembly explosion was removed.  It also removed the
intended packed read-modify-write sequences.  Post-allocation MIR nevertheless
showed the removed state homes being replaced directly by long-lived stack
homes: values assembled from `sim + 136376` and `sim + 136568`, for example,
were stored to stack slots immediately in the comb prefix and carried to the
FF suffix.  The fused frame grew from 37,760 to 39,704 bytes.  A trace-free,
non-LTO CPU-0 run did not reach the semantic completion marker within 260
seconds and had progressed only to the kernel `workingset` initialization.
The complete trial was removed without a commit.

This is an allocator architecture failure, not a reason to add another
promotion threshold.  The present pipeline makes three decisions in the wrong
order:

- `LogicalValue` is exactly one MIR VReg and receives one phi-congruence spill
  home before its actual split ranges exist;
- the Braun--Hack W/S walk chooses register residency and stack operations,
  then a later pass substitutes a MemorySSA recipe for selected operations;
- coloring runs once after reconstruction and cannot request a different
  split, home, or coalescing decision from the plan which created its live
  ranges.

The replacement keeps SSA and the verified machine backend, but introduces an
explicit allocation model with four separate identities:

1. A **machine value** is a 32- or 64-bit MIR result.  No arbitrary HDL width
   is attached to a VReg.
2. A **logical state value** is a range/version identity in StateSSA and
   MemorySSA.  It may be materialized by one or more machine values.
3. A **live bundle** is one connected, independently splittable subset of a
   machine value's live range, with exact instruction uses and phi-edge uses.
4. A **home** is one way to recreate a bundle: a physical register, a colored
   stack slot, a MemorySSA-proved state recipe, or a pure rematerialization
   DAG.  Home validity and home cost are properties of a bundle and its use
   cluster, not global properties of the source VReg.

The production algorithm will follow the live-interval/splitting structure
used by modern optimizing compilers rather than adding cases to the current
single forward walk:

1. Assign stable slot indexes to block entries, instruction uses/defs, exits,
   and phi edges.  Build exact sparse live segments and use lists over the
   complete CFG.  Record loop/SCC nesting and block-frequency estimates
   separately from semantic liveness.
2. Lower fixed-register and pseudo-clobber constraints before the final
   allocation decision.  Build per-register live-interval unions so an
   assignment is checked against actual interference, not only scalar pressure.
3. Process live bundles by spill weight.  Try a free register, bounded
   recoloring, and eviction of cheaper bundles.  If none succeeds, split at
   dominance, loop, and use-cluster boundaries and return the children to the
   work queue.  A split is an SSA rewrite with explicit edge values, not a
   textual reload insertion.
4. Choose register, state, stack, or rematerialized residency for each child by
   the complete cost of its guaranteed use cluster.  A state recipe may be a
   verified multi-load/shift/merge DAG.  The decision includes writeback and
   transition costs, so eliminating a packed store cannot silently create a
   second mandatory stack home.
5. Rebuild only affected intervals after splitting, then allocate again until
   every bundle is assigned.  Coalesce copies/phis when the merged interval is
   colorable; color stack slots from the final spilled-interval interference
   graph.
6. Reconstruct MIR from the selected homes and independently recompute CFG
   liveness, physical interference, MemorySSA versions, state bit ranges,
   fixed-register constraints, and spill-home dominance.  The verifier must
   not consume cached facts from the allocator's decision procedure.

Implementation slices are deliberately architectural and each has its own
tests before generated code is allowed to change:

- **27a:** exact slot indexes, live segments, instruction/phi-edge use lists,
  interference queries, and an independent dataflow verifier;
- **27b:** live bundles and a home graph containing stack, unary remat, exact
  state, and multi-load state recipes.  Physical home identity is independent
  of MemorySSA version, while every covered use retains its own exact versioned
  materialization proof;
- **27c:** interval-union allocation, eviction, bounded recoloring, and
  dominance/loop/use-cluster splitting behind a diagnostic implementation
  selector;
- **27d:** constraint lowering, phi/copy coalescing, stack-slot coloring, and
  complete post-allocation verification, followed by replacement of the W/S
  planner only after differential MIR execution passes;
- **27e:** expose range StateSSA values to this allocator and enable lazy
  packed writeback.  No broad cross-phase load/store removal is enabled before
  the allocator can choose all four homes for every resulting live bundle.

Step 27b now implements the allocation-owned root bundles and HomeGraph without
connecting them to the production allocator.  Every candidate records its
exact covered instruction/phi-edge uses.  Materialization has two deliberately
separate identities: an interned, version-independent shape DAG groups the same
physical home across uses, while each use points to an exact recipe DAG whose
state leaves retain the MemorySSA versions proved at that point.  A disjoint
RMW therefore changes the exact recipe but does not create a false home
boundary.  Multiple physical fragments are assembled with explicit shift,
mask, and OR nodes.  Static-store provenance records source and physical bit
offsets but does not attach an HDL width to a VReg.  Partial-fragment metadata
is invisible to the legacy W/S planner; only the HomeGraph consumes it.

The focused tests cover a two-load reconstruction, a source-range hole,
overlapping invalidation, path-specific phi-edge validity, and independent
rejection of a corrupted use set.  The disjoint-RMW regression additionally
proves that two uses share one home shape while retaining distinct exact
MemorySSA recipe IDs.  To prove that the legacy path did not change,
`bbd6ed81` and the candidate were built in separate non-LTO worktrees and given
the same ordered source list, working directory, and O2 configuration.  All
four complete outputs were byte-identical:

- pre-optimized SIR: `38190d2c41df1f9b00df308f6249132a8ac511817f9fc03c6b8341714f9a383e`;
- post-optimized SIR: `e21dd4020124c4ad0c8796be6a2c950db285ea398a2d5b0cfe9604bc17c8fca4`;
- native-optimized SIR: `ca588bdf0a97d76c4c5c164a0b8bb1f4503c3f40b2de3512fed6d6ebb062538a`;
- complete MIR: `a2c7746d55bf4bbdbf1454763177dd055694db8cbf058584174924e83b123741`.

The retained candidate trace is
`target/heliodor/analysis/step27b-home-graph-v2-20260718`; its parent comparison
is `target/heliodor/analysis/step27b-parent-bbd6ed81-v1-20260718`.  The older
Step 26d trace used a different source registration order and consequently
already differed in pre-optimization SIR, so it was not used to infer a backend
change.

Step 27c1 adds the physical-register interference matrix used by the
replacement allocator.  Each register owns an ordered interval union per CFG
block rather than one layout-linearized range.  Queries touch only blocks
present in the candidate's sparse segments, so mutually exclusive branch arms
can share a register while same-block overlaps remain exact.  Assignment and
removal update a bidirectional bundle/register map transactionally.  The same
structure computes maximal free segment differences for later region
splitting, and an independent verifier rebuilds every ordered union from the
bundle memberships.  This slice supplies allocation mechanism only; it does
not yet choose registers or alter production MIR.

Step 27c2 layers the first complete-bundle allocation policy on that matrix.
The work queue is ordered by exact home-loss/live-length ratio using integer
cross multiplication.  It first tries a free register, then transactionally
recolors the target register's resident neighborhood.  Recolor depth and work
are bounded by the physical register-file size, so compile time cannot depend
exponentially on an unbounded RTL live set.  If recoloring fails, an original
bundle may evict a strictly cheaper resident set; displaced bundles advance to
an `Evicted` stage which cannot initiate another eviction.  This monotonic
stage transition prevents oscillation.  The fallback is the cheapest
register/stack/rematerialization/state candidate which covers every bundle
use, retaining exact per-use recipes.  An independent plan verifier rebuilds
the interval matrix and every selected home from the HomeGraph.  Region
splitting and transition placement remain the next 27c slice, so this
diagnostic policy is still disconnected from production allocation.

Step 27c3 removes the remaining VReg-wide fallback from home choice.  For a
given use subset, the solver evaluates both no-stack residency and creation of
one shared stack home.  Within either case it selects the cheapest exact
state/rematerialization recipe or stack reload independently at every use,
then groups equal home shapes into allocation children.  Because non-stack
homes have zero creation cost and stack has one shared creation cost, this is
an exact solution for the current HomeGraph rather than a greedy set-cover
threshold.  A regression in which a state load is valid before an overlapping
write but not after it produces a state child for the first use and a stack
child only for the second.

Step 27c4 makes splitting an allocation decision.  After free-register,
recolor, and eviction attempts fail, each physical interval union subtracts
its occupied sparse segments from the bundle.  Free pieces are connected only
across real CFG exit/entry edges.  A candidate region begins at an existing
instruction use, materializes the exact recipe proved at that use, and includes
only reachable later uses which that entry dominates.  A reverse slice through
the free-piece graph retains only segments needed to connect those uses.
Register and home children then partition the root uses exactly once; stack
children and transitions share one logical stack-home creation.  The split is
accepted only when its exact transition plus remainder-home cost is lower than
the unsplit home cost.  Per-block ordered indexes keep use/edge lookup
logarithmic in the number of free pieces in that block.

Focused regressions cover a same-block free suffix, a connected multi-block
region, and a diamond where a transition on one arm must not claim uses from
its sibling.  The plan verifier independently checks transition recipe
validity, point dominance, child segment containment, exact use partitioning,
shared stack identity, and a freshly rebuilt physical interval matrix.

Step 27c5 begins the actual-scale correction of that diagnostic design.  The
first Heliodor connection exposed two independent complexity defects rather
than a tuning issue.  HomeGraph stored homes transposed by physical shape, so
selecting a home for a use searched every candidate and then searched that
candidate's use list.  Its verifier also nested complete live-interval and
all-use MemorySSA reconstruction, causing the same MemorySSA to be built up to
four times while native functions were compiled concurrently.  HomeGraph now
owns a direct use-to-exact-recipe index.  Register and stack residency are
allocator mechanisms and are implicit; only use-local state/rematerialization
proofs occupy the graph.  Each producer verifies its own dataflow once, and
the HomeGraph boundary verifies bundle ownership and recipe-DAG structure
without recursively rebuilding its inputs.  A later post-allocation boundary
still has to re-derive every selected home from MIR before this allocator can
replace production allocation.

The CPU-0 non-LTO Heliodor diagnostic after this change built the four large
HomeGraphs far enough to report 8.128 s for `eval_comb`, 6.755 s for
`eval_only_ff`, and 6.611 s for `eval_apply_ff` while those compile threads
shared one CPU.  The run was then deliberately stopped: allocation itself did
not finish in the following 50 seconds and process RSS reached about 5.1 GiB.
This rejects the current cloned interval matrix and per-use free-region search;
it is not a compile-only success or a Linux correctness result.  The next
slice replaces those structures with shared sparse interval storage,
transactional undo, and one region dataflow per register.

Step 27c5b replaces those rejected structures.  All physical-register unions
share one immutable CFG index, while each allocation bundle owns a
`SparseRange` which resolves `BlockId` to the corresponding CFG row and proves
segment order, membership, and self-noninterference exactly once.  Register
queries accept only a token tied to that CFG index; the raw diagnostic API is a
checked adapter to the same indexed implementation rather than a second query
semantics.  Per-register block trees are sparse, conflict collection reuses a
dense epoch table, and one register probe supplies the free, recolor, eviction,
and split decisions for a bundle.  Recoloring uses an explicit undo journal
instead of cloning the interval matrix, and failed/error exits restore the
matrix exactly.  Free-region splitting builds one sparse graph per register,
partitions it into disjoint dominated owners, and visits each free node once
across the resulting candidates.  Displaced roots and split leaves enter a
monotonic `NoEvict` stage, which prevents a finalized child from re-entering an
unsupported eviction chain.

The clean CPU-0 non-LTO diagnostic compile-only run retained at
`target/heliodor/results/20260718T143501Z_celox_test_soc_linux_boot.log`
completed with `compile_ns=178890305762` and `execute_ns=0`.  Allocation took
21.582 s for `eval_apply_ff`, 25.729 s for `eval_only_ff`, 99.052 s for
`eval_comb`, and 79.597 s for `eval_comb_apply_ff`.  The corresponding prior
run took 240.678 s overall and 23.916 / 28.367 / 155.144 / 137.980 s in those
four allocation phases.  This proves termination and removes about 61.8 s of
diagnostic compile time, but it does not qualify the allocator or establish a
Linux result: the legacy production allocator still emits the MIR.

Two debugger stops on the unchanged prebuilt candidate identify the remaining
architectural work.  One large function was restoring resident ranges through
`MatrixTransaction::rollback -> assign_validated -> overlapping_entries_at`
after a failed recolor; the fused function was scanning
`collect_conflicts_validated` while probing registers.  The next slice must
therefore replace destructive speculative recoloring and repeated
bundle-by-register range scans.  Retaining the present search and making its
ordered maps incrementally cheaper is not an acceptable completion path.

Step 27c5c removes both of those operations from unsuccessful allocation
attempts.  A bundle first asks each register only the early-exit availability
question.  Complete conflict sets are materialized lazily only after no free
register exists, and the resulting per-register cache is shared by recolor,
eviction, and splitting.  Recoloring is planned against the immutable matrix.
All residents which conflict with one candidate on a target register are
already pairwise noninterfering, because they coexist in that register's
verified interval union.  Consequently each resident can choose an available
alternative independently, including the same alternative as another
resident.  Recursive search, speculative remove/assign, and failed rollback
are unnecessary.  The complete move set is committed once; the undo journal
is retained only to make an unexpected commit error atomic.  The old limit
which rejected a recolor merely because it contained more residents than
physical registers was also removed: it belonged to the exponential search,
not to this linear plan, and retaining it would discard legal allocations.

The retained clean CPU-0 non-LTO run is
`target/heliodor/results/20260718T145639Z_celox_test_soc_linux_boot.log`.
It completed with `compile_ns=131862840713`, `execute_ns=0`, and allocation
times of 26.015 s for `eval_apply_ff`, 30.538 s for `eval_only_ff`, 55.274 s
for `eval_comb`, and 29.845 s for `eval_comb_apply_ff`.  A bounded intermediate
build completed in 129.844 s with 20.833 / 25.385 / 52.782 / 26.615 s, but was
not retained because its physical-register-count cutoff rejected otherwise
legal linear recolors.  Relative to Step 27c5b, the retained design removes a
further 47.0 s overall and 43.8 / 49.8 s from the two largest allocation
phases.  This remains a diagnostic compile-only result; production MIR and
Linux execution are unchanged.

An unchanged-binary debugger stop after this redesign found three active
allocation threads in `try_split` (free-region construction, use-to-segment
lookup, and temporary-region destruction) and one in lazy conflict
materialization.  The next architectural boundary is therefore an
allocation-owned split topology: CFG edges and exact use ownership are built
once per bundle, while each physical register contributes only occupancy cuts.
Rebuilding a complete free-region graph for every bundle/register pair is the
next rejected design, not a loop to tune.

Step 27c5d implements that boundary.  Split topology belongs to the immutable
root bundle and is built lazily only when allocation reaches region splitting.
Its canonical nodes own the block mapping, exact use ownership, dominance-
ordered instruction seeds, block entry/exit slots, and cross-block CFG edges.
The cached topology is then shared by every physical-register probe.  A probe
subtracts that register's occupied intervals, projects the resulting free
fragments onto their canonical nodes, and keeps only canonical edges whose two
boundary slots remain free.  It no longer walks CFG successor rows, resolves
all exact uses to live segments, or sorts the same transition seeds once per
bundle/register pair.  Split children cannot acquire another topology: only
immutable roots enter this analysis and children remain final register/home
leaves.

The retained CPU-0 non-LTO diagnostic is
`target/heliodor/results/20260718T151058Z_celox_test_soc_linux_boot.log`.
It completed with `compile_ns=129857861879`, `execute_ns=0`, and allocation
times of 26.500 s for `eval_apply_ff`, 31.208 s for `eval_only_ff`, 53.007 s
for `eval_comb`, and 25.172 s for `eval_comb_apply_ff`.  Against Step 27c5c,
the critical compile interval improves by only 2.005 s; the two largest
topology-heavy phases improve by 2.266 s and 4.673 s, while the other two vary
upward by 0.485 s and 0.670 s.  This validates topology reuse as a bounded
architectural slice but rejects it as the performance solution.  Production
MIR remains generated by the legacy allocator, so this compile-only diagnostic
is not a Linux semantic result.  The next slice must identify and replace the
remaining allocation-wide work rather than tune fragment maps or search
thresholds.

Step 27c5e removes another allocation-wide rebuild.  Every immutable root now
owns one `RootHomePlan` containing the best exact choice at each use with and
without stack availability plus additive totals for the complete use set.
Initial queue cost reads the complete total directly.  A split candidate
subtracts its register-covered rows to price the complement, and carries only
the entry use and site rather than constructing a grouped `HomeSelection` for
every candidate.  Recipe vectors and `HomeKind` groups are materialized only
for the winning transition and final home children.  The plan verifier builds
an independent root-plan table once and reuses it while reconstructing every
expected child; it no longer calls the old two-mode partition builder at each
verification site.

The retained CPU-0 non-LTO diagnostic is
`target/heliodor/results/20260718T152639Z_celox_test_soc_linux_boot.log`.
It completed with `compile_ns=126211061125`, `execute_ns=0`, and allocation
times of 24.600 s for `eval_apply_ff`, 28.062 s for `eval_only_ff`, 51.346 s
for `eval_comb`, and 23.031 s for `eval_comb_apply_ff`.  All four allocation
phases improve by 1.901 / 3.146 / 1.661 / 2.141 s respectively, and the
critical compile interval improves by 3.647 s from Step 27c5d.  This confirms
that repeated home reconstruction was real work, but a 51.346 s allocation
phase remains.  Further work therefore moves to free-range/candidate search;
the root home policy is not to be tuned with cutoffs.  Production MIR still
uses the legacy allocator, so this remains a diagnostic compile-only result.

Step 27c5f unifies interference and split projection instead of accelerating
two separate searches.  Once availability has failed, one staged physical-
register query now returns both the unique conflicting bundles used by
recolor/eviction and exact occupied cuts indexed by the candidate's canonical
segment number.  The register probe owns both results under one validity
invariant.  If recolor and eviction fail without mutating the matrix, splitting
consumes those cached cuts directly.  It no longer searches the same ordered
interval union again, allocates a free-range difference there, resolves each
free segment's block, and maps it back to a canonical topology node.

Split selection is also streaming.  The free graph computes dominance,
covered uses, the reverse slice, and exact home cost first.  Candidates which
cannot beat the unsplit home or the current global incumbent are discarded
before allocating a segment vector.  Only an improving candidate materializes
segments and clones its use subset; there is no per-register vector of fully
formed candidates.  The same-block, cross-block, and sibling-arm regressions
continue to exercise the resulting CFG semantics, while the interval-union
test independently checks that the staged query reports the same conflict set
and exact canonical cuts.

The retained CPU-0 non-LTO diagnostic is
`target/heliodor/results/20260718T153550Z_celox_test_soc_linux_boot.log`.
It completed with `compile_ns=111349015315`, `execute_ns=0`, and allocation
times of 21.190 s for `eval_apply_ff`, 23.277 s for `eval_only_ff`, 30.433 s
for `eval_comb`, and 11.727 s for `eval_comb_apply_ff`.  Relative to Step
27c5e, those phases improve by 3.409 / 4.785 / 20.913 / 11.303 s and the
critical compile interval improves by 14.862 s.  This is the first retained
27c5 slice after immutable recoloring with a large improvement from one
allocator design change.  Production MIR remains on the legacy allocator, so
the result is still compile-only and makes no Linux semantic claim.

A final container-order trial after Step 27c5f removed the conflict-ID sort
and made recolor/eviction consume first-interference order.  The complete
CPU-0 non-LTO compile-only run is
`target/heliodor/results/20260718T154524Z_celox_test_soc_linux_boot.log`.
It regressed from 111.349 s to 135.893 s even though one function improved;
other functions incurred substantially worse sparse resident/home access.
The complete code change was removed.  This closes conflict-container order,
map thresholds, and similar local query tuning as the continuation of Step
27c.  No commit or production-MIR change resulted from the trial.

Step 27d1 starts the production boundary instead.  Inspection of the complete
diagnostic plan exposed two correctness obligations which cannot be delegated
to a later rewriter.  `stack_home_created` does not identify any store which
reaches the selected reloads.  More fundamentally, a home child has an empty
live range even though an executable definition-to-store, reload-to-use, or
state/rematerialization recipe needs one or more physical registers.  Those
synthetic values may interfere with retained root ranges; assigning an
unmodelled scratch register after allocation would make the allocation
incomplete.

The retained slice introduces an off-to-the-side allocation IR.  It copies
only original machine def/use and phi-edge identities, keeps stable anchors to
the immutable input MIR, and can insert stack stores, stack reloads, and exact
recipe nodes without mutating `MFunction`.  Every synthetic instruction and
machine value has a checked dense identity and exactly one definition.
Original instruction order remains independently verified, malformed
synthetic def/use signatures are rejected before mutation, and a failed build
cannot expose partial MIR.

Exact liveness is shared rather than reimplemented.  The existing Step 27a
analyzer now consumes a minimal strict-SSA program interface implemented by
both `MFunction` and the allocation IR.  Slot construction, block fixed point,
phi-edge uses, sparse segments, definition dominance, and the independent
equation verifier are therefore identical for original and synthetic values.
Focused tests prove unchanged MIR has exactly identical intervals, a
definition-to-stack-store forms a real short range, reload and multi-step
recipe results re-enter liveness, one phi-edge reload is confined to its
normalized edge, malformed insertion is atomic, and the source `MFunction`
remains unchanged.  This slice does not yet connect home selection to the
allocation IR or change production MIR, so it makes no Linux or execution-time
claim.

Step 27d2 adds the independent stack-home proof required before any plan may
insert reloads.  It scans the actual ordered synthetic operations in the
allocation IR, selects only homes which have a reload demand, and constructs
sparse Boolean SSA from their store-definition blocks plus the explicit false
definition at function entry.  Iterated dominance frontiers contain the AND
meets; a dominance-tree rename resolves stores and reloads in exact
instruction order, and a final fixed point propagates every false phi input.
The representation is proportional to stores, reloads, and placed sparse phis,
not the product of CFG blocks and all stack homes.

The verifier accepts a join reload only when both incoming arms store the same
home.  It rejects a store on one arm and also rejects a same-block store which
occurs after the reload.  Phi-edge synthetic operations are accepted only on a
dedicated normalized edge block, so treating them as ordered block operations
cannot move a store or reload onto a sibling path.  This slice still does not
lower `AllocationPlan` homes or change production MIR; it establishes the
all-path contract which the next home-expansion slice must satisfy.

Step 27d3 consumes the diagnostic `AllocationPlan` only as a home and split
placement proposal.  It expands every selected stack, state, rematerialization,
and register-entry transition into the off-to-the-side allocation IR.  A stack
root receives an explicit definition-to-store range and each reload defines a
new reload-to-use value.  Every reachable node of an exact state/rematerialize
recipe defines its own machine value.  A split register child is represented
by one synthetic SSA value shared by its complete dominated use cluster.  The
old physical register is a preference, not an assignment which synthetic
interference may invalidate.

After all insertions, expansion runs the sparse all-path stack proof and the
shared exact liveness analysis over original and synthetic values together.
Input-MIR use anchors are resolved separately from their new allocation-IR
positions: inserted instructions change both local instruction indexes and
later global slots, while an inserted phi-edge operation changes the
predecessor exit slot.  The verifier requires every rewritten exact use to
appear in its replacement interval and leaves the source `MFunction`
unchanged.  Focused pressure tests force a real stack home, a point-specific
state/stack partition, and a dominated split register region.  This slice
still does not change production MIR or claim a Linux/timing result.  Step
27d4 must build one allocation queue from all recomputed intervals and remove
the one-register-child finalization rule.

Step 27d4 builds that joint allocation boundary.  Every original or synthetic
machine definition with a live interval enters one stable value table.  Exact
register-region use subsets are reconstructed from the expanded per-use map;
all definition-to-store, reload-to-use, recipe-intermediate, and dead-result
machine ranges remain fixed transition values.  Old physical assignments are
checked target-register affinities only.  A stale interval snapshot, orphaned
region, mismatched exact use set, or non-bijective value index is rejected
before coloring.

The allocation walk follows dominator-tree definition order and queries the
same CFG-sparse physical interval unions used by the diagnostic allocator.
Thus sibling-arm ranges can share one register without a layout-linear live
interval.  Success produces a total assignment which is independently rebuilt
in a fresh sparse matrix.  Failure returns the blocked definition, every
conflicting resident on each physical register, and the exact root regions
eligible for splitting.  If all conflicts are already fixed transition
ranges, allocation reports a producer error instead of inventing an
unallocated scratch.  Focused tests cover affinity conflicts, complete
original-plus-synthetic enrollment, sibling-arm sharing, a synthetic-pressure
split request, and unsplittable local pressure.  Production MIR is still
unchanged.  Step 27d5 must cost the returned alternatives, split a selected
root into strictly smaller CFG-connected use regions, materialize new homes,
and rerun this same joint allocation to a finite fixed point.

Step 27d5 closes that fixed-point loop without restoring the diagnostic
one-register-child fallback.  The blocked definition is the exact cut: every
earlier SSA range which conflicts with the newly processed range must cover
that definition.  For each returned root region, the splitter traverses only
candidate segments which are live across real CFG exit/entry edges.  It moves
uses reachable from the cut, retains the prefix, and partitions the moved set
among earliest dominating instruction uses.  A sibling arm is never reached
from a cut inside the other arm.  If a backedge returns to the cut block,
next-iteration uses before the cut are included but forced to exact
materializations because their static sites dominate the cut.  Phi-edge entry
uses are likewise singletons until atomic lowering can create synthetic phis.

The immutable `RootHomePlan` prices only the resulting region entries and
accounts for an already-created root stack home.  A selected stack transition
creates one identified definition-to-store operation; joint allocation treats
that store as a fixed use while retaining ordinary root uses as the splittable
region.  Every new reload/state/rematerialization output and every retained
multi-use region returns to the same joint allocation problem.  Applying a
split occurs on a clone, recomputes the stack proof and exact liveness, and
publishes only after independent region ownership and joint-problem rebuilding
pass.

Termination is structural.  Existing synthetic regions record their immutable
entry use and may not recreate the same use set at that boundary.  Every
accepted iteration lexicographically decreases pairwise co-resident region
uses, original-register uses, or total register uses.  Replaced pure synthetic
reload/recipe DAGs are eliminated and their value/instruction identities are
compacted before reallocation, preventing dead transitions from becoming
fixed-only pressure.  Focused tests cover synthetic pressure through complete
joint reallocation, one-arm reachability, loop reentry, a retained register
prefix plus fixed stack store, and repeated-entry termination.  Production MIR
is still unchanged, so this slice makes no Linux or timing claim.

Step 27d6 closes the rewrite boundary. The allocation IR now retains the full
immutable source instruction, not merely its def/use row; lowering rejects a
changed opcode, width, immediate, operand, phi row, CFG edge, or VReg domain.
Synthetic constants, state loads, stack operations, and every width-explicit
pure recipe operation lower through one canonical MIR mapping. The complete
result is built in a private `MFunction`, canonical MIR verification runs, and
an independent liveness reconstruction must reproduce the exact expanded
allocation before anything can be published.

The first complete diagnostic library gate exposed an out-of-SSA boundary
which strict-SSA register liveness alone cannot represent. Thirty-two phi
sources materialized before one edge, and then thirty-two phi destinations at
one block entry, are not thirty-two simultaneously required registers: stack
and immediate locations are legal parallel-copy inputs and stack slots are
legal destinations. The retained model therefore keeps semantic phi rows in
MIR while excluding explicitly non-register sources and destinations from
physical liveness. Immediate and persistent-stack sources become exact,
destination-qualified edge locations. A nontrivial state/pure recipe is
materialized into an explicit edge-local stack home, and all of its temporary
machine values still re-enter joint allocation. A stack-resident phi
destination is defined directly by every incoming out-of-SSA copy rather than
by a fictitious register definition followed by a store.

Lowering converts the joint assignment plus these locations into an
`AssignmentMap`, constructs the complete SSA-destruction parallel-copy plan,
and independently verifies both that plan and its frame bounds. The
`interval-diagnostic` driver now executes HomeGraph construction, initial home
planning, explicit expansion, splitting to the joint fixed point, atomic MIR
lowering, filtered physical-liveness reconstruction, and SSA-destruction
verification as one gate. Production MIR remains on the interim allocator, so
this step makes no Linux or throughput claim. Step 27d7 integrates target
constraints, copy/phi coalescing, and final stack-slot interference coloring
into the same closed result before any production switch.

Step 27d7a integrates target constraints without globally pinning a long live
range. A private diagnostic clone is split at every fixed-operand or clobber
point by an explicit complete-live-set SSA permutation. Unlike the old
post-spill entry point, allocation-time permutation verification accepts more
than K rows and hands them to ordinary home selection and joint splitting.
After each allocation-IR rewrite, fixed uses are rebuilt from the immutable
opcode and current operands, while RAX/RDX-style clobbers remove colors only
from sparse intervals live across both sides of the exact instruction.

Copy and register-resident phi edges now form weighted affinities. They affect
initial register order and a transactional conservative coalescer which
temporarily removes both endpoints from the physical interval matrix. A
common color is published only when masks permit it, the sparse union remains
interference-free, and satisfied incident affinity weight strictly increases.
The constraint model and final assignment are independently rebuilt and
verified. The complete diagnostic library, native-testbench, and counter
gates pass. Production MIR is unchanged, so there is no Linux or throughput
claim.

Step 27d7b models stack homes as location-level strict SSA rather than assigning
one frame row per logical home. `AllocationIr` exports exact current positions
for every stack store/reload and stack-resident phi. Stores and stack phis are
definitions; reloads and direct phi-edge stack locations are uses. The latter
enter the shared liveness engine as location-only edge uses, so CFG equations,
definition dominance, loop coverage, and mutually exclusive arms are handled
by the same sparse verifier as machine VRegs. Stack and machine liveness must
produce identical block-slot layouts.

Definition/dominator-order coloring uses a dynamically growing sparse interval
matrix: an existing 64-bit slot is reused only when its complete per-block
union does not interfere, otherwise one new slot is added. The completed map is
rebuilt independently before byte offsets and frame size are published.
Focused tests prove both reuse and separation, while the existing more-than-K
phi-edge test exercises stack destinations and direct edge locations through
atomic lowering. Production MIR is still unchanged; this structural step makes
no Linux or throughput claim.

Step 27d8 exposes the completed replacement result through the explicit
`CELOX_REGALLOC_IMPL=interval` execution mode without changing the default.
The first executable 32-phi regression found that the final verifier still
treated every semantic MIR phi source as a physical-register edge use even
when the destination-qualified allocation row was Stack or Immediate. Final
physical liveness is now reconstructed from the exact assignment locations;
filtering one row never filters another row which happens to share its source
VReg. The candidate passes that JIT regression and the complete common suites.

The first full non-LTO Heliodor attempt did not reach execution. It timed out
after 900.256 seconds with no `CELOX_TEST_RESULT`; the log contains only the
test configuration, and three of four compilation workers were still
continuously active. Consequently this is neither a Linux failure nor a
generated-code timing result. It rejects the current joint fixed-point driver
at actual scale. Every accepted split currently clones the complete
`ExpandedAllocationProblem`, compacts all synthetic identities, recomputes
whole-function allocation-IR liveness, independently recomputes it again while
building a new `JointAllocationProblem`, and then restarts physical coloring
from an empty interval matrix. The following loop iteration rebuilds the same
joint problem once more. This directly violates Step 27's requirement to
rebuild only affected intervals.

The next boundary is therefore a persistent allocation session, not another
search-order or threshold change. Synthetic instruction, machine-value, and
region identities remain stable while splitting; a split transaction updates
only changed def/use rows and their sparse intervals, removes/reinserts those
ranges in the existing physical interval matrix, and queues only displaced or
new values. Dead synthetic identities are compacted once at the final atomic
lowering boundary. Complete liveness, constraints, interference, stack SSA,
and out-of-SSA plans are still rebuilt independently once before publication.

Step 27d9a establishes the stable coordinate and identity domain required by
that session. Live-interval slots are now block-local: a segment already owns
its `BlockId`, so inserting one synthetic operation cannot legitimately
renumber every slot in every later block. A regression inserts an instruction
in one predecessor and proves that the unchanged successor's entry, phi, and
exit coordinates remain identical.

Dead synthetic reload/recipe sweeping no longer compacts VRegs or synthetic
instruction IDs after every split. Removed definitions become unused holes;
new transitions receive monotonically fresh identities, and liveness emits no
interval for a removed value. This removes the allocation-wide metadata repair
which previously changed every later identity. The final MIR may retain unused
VReg-number holes; they have no definition, use, assignment, or emitted code.
Independent final liveness continues to reject any live reference to such a
hole. Region identity and the persistent physical matrix remain the next
session slice; this step alone does not rerun the 900-second Heliodor gate.

Step 27d9b makes physical bundle identity equal to the stable session VReg and
separates it from the compact active-value row. A `JointAllocationSession` now
owns one sparse interval matrix, range-token table, assignment table, and
definition-order work queue across region splits. On update it removes only a
dead or byte-different value's old range, creates ranges for changed/new
values, and leaves every unchanged register membership in place. A focused
regression extends a completed problem by one definition and proves that both
existing matrix memberships survive while only the new VReg enters the queue.

This removes restart-from-empty coloring, but the actual-scale compile-only
gate still timed out after 600.292 seconds. Initial HomeGraph construction,
root allocation, and expansion all completed: the largest reported intervals
were 4.695, 15.278, and 19.775 seconds respectively. None of the three
high-pressure functions reported completion of joint reallocation, and all
three workers remained continuously active at timeout. Thus matrix restart was
a real design defect but not the dominant remaining one. `apply_split` still
clones the complete expanded problem, recomputes whole-program liveness (with
its independent verifier), and `JointAllocationProblem::build` recomputes the
same liveness again. The next retained boundary is a block fact index plus
per-value sparse liveness update; no container or threshold tuning is implied.

Step 27d9c replaces that whole-program liveness boundary with an allocation-
session fact index. Definition and use facts are indexed by the physical block
which owns them; in particular, a phi source belongs to its predecessor edge.
One split rescans only blocks whose allocation-IR rows changed, takes the union
of their previous and new resident values, and reconstructs each affected SSA
range by an exact reverse CFG walk from its uses to its single dominating
definition. Block-local slot coordinates and monotonic VReg identities make
unchanged rows directly reusable. Focused regressions independently compare
instruction rewrites and phi-edge rewrites against complete liveness.

Expanded root uses now have an immutable original-block index, so shifted use
sites in changed blocks are refreshed without scanning every HomeGraph root.
The session builder consumes these producer-owned intervals and does not run a
second whole-function liveness proof after every split. Complete allocation-IR
liveness, stack reaching definitions, target constraints, lowered MIR
liveness, and the final physical matrix are still reconstructed independently
at the atomic lowering boundary. Thus verification was moved to the correct
boundary, not removed.

The candidate common gates pass, but this is not yet the complete persistent
allocator design. `ExpandedAllocationProblem` is still cloned transactionally,
target constraints and region ownership are rebuilt globally, and a new joint
problem still walks every active value before the persistent matrix accepts
its delta. Those all-world operations are the next session-owned indexes; this
step makes no Linux execution or throughput claim.

Step 27d9d makes target constraints part of that persistent session. Each
allocation-IR block owns its fixed-use, clobber, copy, and phi-affinity facts.
A split reports physical blocks whose instruction positions changed separately
from semantic phi-successor blocks whose source rows changed. Only those fact
rows are replaced. Fixed constraints are indexed by stable VReg, affinity
facts are reference-counted across blocks, and allowed-register masks are
recomputed only for values whose facts or sparse ranges changed. Clobber
queries walk the changed value's own sparse block segments rather than every
function value.

The complete machine-fact model is still built independently at initial and
final publication boundaries. Focused tests shift instruction slots across a
target clobber and rewrite a phi predecessor source, then require the
incremental model to equal a fresh complete rebuild. Common candidate gates
pass. The split loop no longer rebuilds machine facts or target masks globally,
but joint region/value rows and the transactional expanded-problem clone remain
all-world operations, so this step again makes no Linux timing claim.

Step 27d9e completes the persistent semantic-row update. Register ownership is
indexed by stable VReg and immutable root-use identity. A changed root removes
only its previous ownership rows and installs only its new register regions;
changed/dead/new VRegs replace their compact semantic row, sparse matrix range,
assignment, and dominator-order entry independently. The pending worklist
retains unassigned unaffected values. A focused split requires this complete
differential session problem to equal an independently rebuilt joint problem.

The split fixed point now mutates a private allocation session in place. A
post-mutation invariant failure discards that unpublished session and returns a
compiler error; it cannot alter source MIR. This removes the complete
`ExpandedAllocationProblem` clone from every iteration while preserving atomic
MIR publication. Synthetic-definition IDs index a local reference-count DCE:
after rewritten liveness is known, only the newly dead value and recursively
dead synthetic operands are removed. Root progress is recomputed only for the
changed root. Register-region IDs are monotonic, metadata has a stable ID-to-
row index, and replacing one region no longer scans or renumbers every root.

Candidate common gates pass. The former all-world liveness, constraints,
semantic rows, matrix restart, expanded clone, DCE, progress scan, and region
renumbering are now absent from the split loop. The next action is therefore an
actual-scale non-LTO compile/run gate, not another local container tweak.

The first actual-scale compile-only gate reached the split loop but stopped
after 217.609 s with `JOINT_ALLOC.SESSION_REGION_IDENTITY` for `v248304` in
`bb6`. A multi-block register region rewrote every owned operand, while the
split transaction reported only its entry block to incremental liveness.
Ownership therefore observed the complete new region but the exact interval
still described an old later-block use. Step 27d9f makes operand rewriting the
single mutation boundary: every rewrite records its physical fact-owner block
and a phi rewrite also records its semantic successor. Liveness and target
constraints consume that same transaction journal. A cross-block-region
regression requires the differential session to equal a complete independent
rebuild.

The journal candidate passed 224 allocator tests and the complete common
suites. A second identical CPU-0 non-LTO compile-only gate passed the original
failure point. `eval_only_ff` and `eval_apply_ff` completed joint allocation,
atomic lowering, and publication verification, but the run stopped after
266.162 s at `JOINT_ALLOC.UNSPLITTABLE_PRESSURE` for `v165177` in `bb11825`.
This is not a compile or Linux result. It exposes the next allocator design
boundary: fixed transition ranges are currently fed to a greedy coloring walk
and, unlike root regions, have neither a proved pressure-bounded producer nor
a recoloring/spill action. The next slice must classify that exact fixed
pressure and repair the transition/live-range model; changing search order or
adding a threshold is not an acceptable resolution.

Step 27d9g establishes that the reported fixed-only pressure was not a demand
for another color. The blocked value and resident set were phi definitions
whose allocation-IR use lists were empty after exact stack/immediate edge
homes had replaced all of their physical uses. A MIR-wide dead-SSA deletion
trial reached the same location and was rejected: one of those phi results was
still named by a downstream semantic phi row, so removing its definition made
the strict-SSA MIR invalid even though no machine instruction needed its
value. This separates two identities which the previous allocator had
conflated:

- a strict-SSA phi identity may remain in MIR so downstream phi rows retain a
  well-defined source;
- a machine live range exists only when that identity must occupy a register
  or stack destination at some instruction or out-of-SSA copy.

Zero-physical-use phi definitions therefore have no live interval and do not
enter constraint affinities or joint coloring. Atomic lowering marks them as
semantic-only destinations, retains their MIR rows, and emits no incoming
parallel copy. A destination-qualified edge home can still carry such an
identity into a later located phi. `AssignmentMap` now owns the canonical
resolution order for that source location, shared by assignment verification
and SSA destruction; ordinary instruction uses cannot resolve a semantic-only
value and are rejected. Complete and differential liveness independently
agree on the absent machine interval.

The first CPU-0 non-LTO run of this model reached atomic lowering for all
large units but stopped after 476.017 s because the independent completed-
assignment verifier omitted destination-qualified edge homes from its source
lookup. SSA destruction had already resolved the same row successfully. The
retained canonical lookup fixes that disagreement. The identical compile-only
gate at
`target/heliodor/results/20260718T212145Z_celox_test_soc_linux_boot.log`
completed all four units with `compile_ns=456876096868` and `execute_ns=0`.
The trace-free full run at
`target/heliodor/results/20260718T213011Z_celox_test_soc_linux_boot.log`
then powered down at the exact `cy=9ae070 x3=aa pass=1` marker, with
`compile_ns=442109357275` and `execute_ns=267182563217`.

This is a correctness and termination milestone, not a throughput win. The
new allocator executes 2.33 times slower than the retained Step 26a result of
114.833 s. Its emitted `eval_comb` contains 115,377 phi rows and 98,037
effective edge copies. The current constraint lowering creates complete-live-
set SSA permutations around fixed uses and clobbers; that mechanism changes
the physical location of unrelated live values and pays for those changes at
out-of-SSA edges. The next architectural slice must replace those whole-live-
set permutations with position-specific fixed-use/clobber intervals and
independently splittable value fragments. It must move only a constrained
fragment when its adjacent colors differ. Tuning coalescing order, spill
weights, or copy peepholes around the existing permutation graph is not an
acceptable substitute.

### Step 28: Replace whole-live-set constraint permutations with fixed intervals

Step 28a removes `materialize_allocation_constraint_perms` from the production
interval path. A fixed operand now receives one ordinary SSA copy immediately
before its constrained instruction. Its short destination interval carries
the target-register requirement, and copy affinity allows allocation to elide
the move when the source already has that color. Clobbers remain exact
instruction facts; they no longer split unrelated live values, create CFG
blocks, or add one-input phi rows. The former allocation-permutation path is
retained only as a test fixture for the rejected representation.

The fixed-use producer has an independent coverage verifier. It requires every
rewritten fixed operand to have exactly one local copy definition and rejects
an arity mismatch, incompatible requirements for one source, stale block or
instruction identities, and missing or duplicate fragments. Focused tests
prove that a legacy-CL shift copies only its count operand while an unrelated
value remains unchanged, and that an `UDiv` clobber creates no block, phi, or
VReg at all.

The CPU-0 non-LTO compile-only gate at
`target/heliodor/results/20260718T215403Z_celox_test_soc_linux_boot.log`
completed with `compile_ns=384401433198` and `execute_ns=0`. Relative to Step
27d9g, `eval_comb` shrank from 2,560,474 to 650,640 emitted bytes and its
effective edge copies fell from 98,037 to 1,334. The fused unit fell from
98,027 to 1,349 effective copies. Total compile time improved from 456.876 s
to 384.401 s.

The identical trace-free full run at
`target/heliodor/results/20260718T220049Z_celox_test_soc_linux_boot.log`
powered down at `cy=9ae070 x3=aa pass=1`, with
`compile_ns=382284373094` and `execute_ns=124495325614`. Removing the graph,
not cleaning it up after allocation, reduced execution time by 53.4% from
267.183 s. This recovers the pre-replacement allocator's broad performance
level, but it does not close the Veryl gap.

Step 28a is intentionally not the final fixed-constraint model. A value live
through a clobber currently loses the clobbered register from its complete
VReg-wide mask. The next slice must put immutable use-to-def reservations in
each physical-register interval union, report the exact reservation cut in an
allocation failure, and split only the intersecting live-range fragment when
using that register is otherwise profitable. Resolution then inserts one
transition only when adjacent fragment locations differ. This is the remaining
modern fixed-interval boundary; restoring whole-live-set permutations or
tuning the global mask is not an acceptable implementation.

Step 28b uses three ordered subslots for every machine instruction: operand
use, clobber barrier, and result definition. A last-use range ends at the
barrier, a result range starts at the definition, and only a range live through
the instruction spans `[clobber, definition)`. Every target clobber therefore
becomes an immutable physical-register reservation over exactly that interval.
Trying to approximate this with either `[use, definition)` or
`[definition, next)` is incorrect: the former rejects legal last uses and the
latter rejects legal result definitions.

The physical interval union owns both movable bundle entries and immutable
fixed entries, with an explicit owner tag. Interference queries return
owner-qualified occupancy cuts. Recoloring and eviction may operate only on
movable owners; fixed owners can only cause a candidate range to be split at
the reported cut or assigned another color. The constraint model retains a
VReg-wide mask only for true fixed operands (whose producer is already a local
SSA fragment); clobbers no longer mutate a VReg mask at all.

Persistent allocation updates replace fixed entries from changed machine-fact
rows only after changed movable ranges have been removed. Publication verifies
the stored reservations against an independent `AllocationIr::machine_facts`
rebuild and verifies that no retained movable interval overlaps a replacement
reservation. A split request carries the exact pressure point derived from the
occupancy cut, rather than reusing the blocked value's definition. Split
planning then considers each `(region, pressure point)` pair and moves only
owned uses reachable after that point.

Step 28b implements that boundary. `LiveIntervalMatrix` stores explicitly
tagged movable and fixed owners in the same ordered unions. Conflict collection
returns the movable residents needed for recoloring plus owner-qualified cuts;
an immutable owner never enters the eviction set. Focused fixtures prove the
three-subslot last-use/result distinction, exact fixed cuts with no fake bundle,
separation of fixed-use masks from clobbers, incremental reservation identity
after a local slot shift, and a split request whose pressure point is the
clobber barrier rather than the blocked definition.

This architectural replacement deliberately produced no Linux code change for
the current workload. The Step 28a trace at
`target/heliodor/analysis/step28a-local-fragments-1ed9f872-20260718` and Step
28b final-source trace at
`target/heliodor/analysis/step28b-fixed-intervals-final-20260718` have identical
complete artifacts: pre-optimized SIR
`336d6b7bd66ea0c824293dd69c25fe2c7aa9f862b7a070ab73949db0bf3771d4`,
post-optimized SIR
`babcca2ac53a003eaf77dab35ae45faf052802de614b9a660b4d024eeddf5900`,
native SIR
`60b82bc32d0a021dd07f68512b6cb1f874775e34b5945a0927834465f7d97fe4`,
and all pre-/post-allocation MIR, assignments, and disassembly
`504d46eb9ea3d5a5f876b0afcdbd4d9ba664b63dddfa63b34f21cfa4cb028106`.
The adjacent non-LTO compile-only measurements were 53.619 s for Step 28a and
52.540 s for Step 28b, so the earlier 384.401 s sample is not attributed to
this change.

Three trace-free full Step 28b runs powered down at
`cy=9ae070 x3=aa pass=1`. Their execute times were 141.988 s at
`target/heliodor/results/20260718T222850Z_celox_test_soc_linux_boot.log` and
118.501 s at
`target/heliodor/results/20260718T223544Z_celox_test_soc_linux_boot.log`. The
run rebuilt from the final source is
`target/heliodor/results/20260718T224819Z_celox_test_soc_linux_boot.log`, with
`compile_ns=56281979403` and `execute_ns=122963481056`.
Because the complete emitted code is byte-identical and host timing varied by
19.8%, Step 28b makes neither an execution-speedup nor a regression claim.

Step 28c makes the allocation-owned split fixed point genuinely incremental.
The prior differential API still invalidated work by mutable dense instruction
position: inserting one reload changed every later def/use fact in the block,
and the liveness index removed each old use by scanning that value's complete
global use vector. It also materialized a value-by-block membership relation
for the stable allocation IR. On the large combinational units those structures
made a nominally local split revisit most of the function.

Allocation IR now gives every original instruction its immutable source
identity and every synthetic instruction a monotonic identity disjoint from
the original range. Stable order-maintenance slots remain the physical program
coordinates. `UseSite` is ordered by `(block, slot, identity)`, so physical
range queries and exact identity deltas share one total-order contract even
when synthetic IDs do not follow emitted order. Complete liveness construction
and the incremental fact scanner use the same identity mapping; a full
independent rebuild therefore remains an exact oracle.

Each changed block is scanned once. Its sorted old and new definition/use rows
are differenced linearly, and all removals/additions for one VReg are merged
with that VReg's global use row in one linear pass. Unchanged original facts do
not enter the affected set. Stable Allocation IR no longer stores the enormous
value-by-block membership relation; immutable slots guarantee that a value
merely crossing an insertion block keeps the same physical range. Dense-slot
MIR retains the membership relation because a dense relabel really does change
its coordinates. Live-length cost is recomputed only when sparse geometry
changes; a metadata-only relabel retains its interval-union token, assignment,
and session priority.

Target constraints now obey the same invalidation boundary. Block affinity
facts remain reference counted, while a sparse bidirectional endpoint index
and active pair-weight map update only edges incident to an activated,
deactivated, added, or removed value. Stable clobber reservations are flattened
only when a changed block's exact reservation row differs. The update publishes
explicit affinity and reservation revisions, so the joint session no longer
compares or rebuilds either complete vector on every split round. Focused tests
require unchanged synthetic insertion to publish neither revision and a phi
source rewrite to publish only the affinity revision; every incremental model
must still equal a fresh complete rebuild.

The first retained actual-scale sample before the stable fact delta is
`target/heliodor/results/20260719T013959Z_celox_test_soc_linux_boot.log`:
compile-only took 233.097 s, with joint allocation taking 130.419 s for
`eval_comb` and 168.359 s for `eval_comb_apply_ff`. The stable liveness delta
completed compile-only in 173.170 s at
`target/heliodor/results/20260719T020012Z_celox_test_soc_linux_boot.log`.
After incremental affinity/reservation publication, the timed compile-only run
at `target/heliodor/results/20260719T020944Z_celox_test_soc_linux_boot.log`
completed in 163.886 s; the two large joint-allocation intervals were 68.239 s
and 97.147 s. This is a 29.7% reduction from the 233.097 s rejected updater,
not an acceptance result.

The full non-LTO run at
`target/heliodor/results/20260719T021259Z_celox_test_soc_linux_boot.log` then
printed the Linux kernel log through `reboot: Power down` and exactly one
`cy=9ae070 x3=aa pass=1` marker. It reported compile 163.766 s and execute
149.306 s. The compile time is still almost three times the final committed
Step 28b sample and peak RSS remains roughly 5--7 GiB, so Step 28c does not
claim the allocator's scale problem is solved or that generated-code throughput
improved. The next compile-time slice must use optimized sampling to replace
the remaining all-range interval-interference/session ownership work with
persistent sparse indexes. It must preserve assignments for unchanged ranges
and pass an independent final rebuild; local queue ordering, thresholds, or
container substitutions are not substitutes.

Step 28d changes the split mutation boundary rather than tuning those
containers. Optimized sampling found two representations of the same defect:
`AllocationIr::insert_synthetic` shifted the complete dense block once per
synthetic operation, and the liveness updater repeatedly rebuilt block and
VReg use rows before reconstructing the affected sparse range. Stable slots
already made those intermediate dense layouts semantically irrelevant.

An allocation round now stages synthetic rows in a monotonic-ID arena and
publishes each touched block with one ordered merge. The exact producer journal
publishes a final block-layout replacement instead of replaying dense insertion
positions. Definition/use facts are merged once per block; the canonical
immutable use row is shared by the fact index and `LiveInterval`, so range
reconstruction does not retain or copy a second all-use vector. One
epoch-marked CFG workspace is reused across affected values. The former
changed-block rescan remains an independent debug oracle and agrees exactly
with the producer journal in all focused fixtures.

Synthetic order sequences are monotonic per `(block, anchor zone)`, not aliases
of the global synthetic instruction ID. A global ID is a valid total-order tie
breaker but not a valid distance coordinate: insertions at other anchors would
create empty same-zone gaps and distort interval length and spill cost. A
focused regression interleaves two anchor zones and requires local sequences
`1, 2` and `1` independently.

Whole-session split verifiers are now exhaustive development checks, enabled
by debug assertions or `CELOX_REGALLOC_VERIFY`. They are not repeated after
every symbolic split in optimized compilation. This does not weaken the
publication contract: atomic lowering still independently rebuilds complete
liveness, machine facts, assignments, and the physical interval matrix before
publishing MIR.

The successive trace-free compile-only gates completed in 132.604 s after
moving exhaustive proofs to that boundary, 119.497 s after the exact producer
journal, 94.470 s after epoch-marked sparse reconstruction, and 50.045 s after
block-transaction publication and shared use rows. That last compile-only
candidate still used global IDs as same-anchor distance labels; it motivated
the final local-sequence correction and is not an acceptance result. Its record
is `target/heliodor/results/20260719T032436Z_celox_test_soc_linux_boot.log`.
The final-source non-LTO full run at
`target/heliodor/results/20260719T034452Z_celox_test_soc_linux_boot.log`
printed through `reboot: Power down` and exactly one
`cy=9ae070 x3=aa pass=1`, with 56.294 s compile and 127.017 s execute. The
compile interval is 65.6% below Step 28c's 163.766 s full-run interval and is
again in the Step 28b range. Execution remains inside the previously observed
host variation, so this step makes no generated-code speed claim.

All 240 focused allocator tests, all 860 interval library tests, 60 non-ignored
native-testbench tests, and 9 non-ignored counter tests pass, together with
all-target strict clippy, formatting, and diff checks. This closes the
allocation-session update complexity defect; peak RSS still requires a
separate retained measurement.

The next allocator slice must change the generated code at its primary spill
boundary. Today `allocate_roots` chooses persistent homes, `expand` commits
stores/reloads, and only then does joint physical allocation discover the
actual interference. That two-stage decision is unlike an integrated modern
allocator: it cannot compare keeping a value in a register, splitting it at a
specific pressure interval, and spilling only that fragment in one cost
problem. Step 29 therefore replaces eager home expansion with allocation-owned
fragments and SSA spill placement. A stack home is created only for a fragment
selected by the final matrix; reload placement uses the fragment's exact use
frontier, and adjacent register fragments receive a move only when their final
colors differ. Local spill-weight or copy-order tuning is not an acceptable
substitute.

Step 29a first removes a scalability bug at that boundary. A coloring failure
previously reduced each physical register to one occupancy cut. Splitting one
cut, publishing the changed allocation IR, and then discovering another cut on
a sibling CFG arm made the number of liveness/constraint publications depend
on branch count. Conversely, collecting every occupied interval and treating
all cuts as interchangeable loses the physical color: an early conflict in
one register must not erase a long free prefix in another.

The joint allocator now grows the candidate from its SSA definition through
the free part of each physical register independently. A block suffix query in
the interval union finds its first exact occupancy in logarithmic time. A free
block propagates to live CFG successors; every occupied successor path adds a
cut to that register's frontier. The definition block is visited separately at
its definition and at a loop re-entry, so a backedge cannot hide a pre-
definition conflict. Epoch-indexed block/segment tables are retained by the
allocation session and block entry/exit slots are stored without cloning the
per-instruction slot vectors.

Split planning compares at most one plan per candidate and physical register.
All cuts in that register frontier seed one sparse CFG traversal, and the union
of reachable owned uses is materialized in one allocation-IR transaction. A
debug verifier independently recomputes the moved set from every individual
cut and requires the same union. Frontiers from different registers are never
combined. Fixed blocked values also avoid spilling movable residents on a
register whose frontier already contains an immutable blocker.

The optimized non-LTO `interval-diagnostic` compile-only gate now completes in
98.538 s; the complete `interval` trace is at
`target/heliodor/analysis/step29b-multicut-interval-publish-20260718` and took
97.362 s. The full run at
`target/heliodor/results/step29b-multicut-interval-linux-20260718.log` printed
through `reboot: Power down` and the exact
`cy=9ae070 x3=aa pass=1` marker, with 94.492 s compile and 138.064 s execute.
The generated fused function's spill frame is `0x4000` bytes versus `0x93b0`
in the inspected established-allocator trace, but its emitted body is larger
(`0x310b1f` versus `0x2ec15e`). The execution sample is also above Step 28d,
so Step 29a claims allocation completion and exact CFG semantics, not a runtime
speedup. Integrated multi-fragment coloring and final spill/reload placement
remain Step 29's code-quality boundary.

Focused regalloc tests pass 244/244, including two-arm frontier construction
and one-transaction multi-cut splitting. The complete library passes 864/864,
native testbench 60 passed with 1 ignored, counter 9 passed with 3 ignored, and
all-target strict clippy and formatting pass.

Step 29b connects that frontier decision to physical allocation. Step 29a
proved a retained prefix free in a particular register, but stored the register
only in `RegionSplitPlan`. Split publication kept the source region's previous
preference, rebuilt the shortened allocation row as unassigned, and asked the
generic color loop to rediscover a decision whose proof had just been thrown
away. The selected color therefore affected where the suffix was split but not
where the retained fragment was allocated.

Split mutation now changes every retained use and its stable register-region
metadata to the selected color as one ownership fact. After the whole symbolic
round updates liveness, constraints, fixed reservations, and semantic rows, the
persistent allocation session checks the final shortened sparse range against
the updated physical matrix and inserts it in that exact color. A sibling
fragment or new fixed reservation may have occupied the range while the round
was published; in that case the fragment remains pending for normal coloring
and no overlapping assignment is installed. Focused tests cover original and
synthetic region metadata, the rebuilt semantic row, the actual matrix
membership, and the newly-blocked fallback.

The complete optimized non-LTO trace is at
`target/heliodor/analysis/step29c-retained-color-interval-20260718`. Its
pre-optimized, post-optimized, and native-optimized SIR files are byte-identical
to Step 29a, while the MIR differs only after register allocation. The emitted
`eval_comb` endpoint falls from `0x1d70f6` to `0x1d6ceb` (`0x40b` bytes), and
the fused `eval_comb_apply_ff` endpoint falls from `0x310b1f` to `0x3106be`
(`0x461` bytes); both retain the same `0x4000`-byte frame. The dump compile took
100.486 s. The full run at
`target/heliodor/results/step29c-retained-color-interval-linux-20260718.log`
printed through `reboot: Power down` and the exact
`cy=9ae070 x3=aa pass=1` marker, with 95.104 s compile and 133.710 s execute.
That execution sample is 3.2% below Step 29a but is not a substantial or
repeat-qualified speed claim. Eager suffix home expansion still prevents one
solver from comparing all register fragments and spill placements together;
that remains Step 29's next architectural boundary.

Focused regalloc tests now pass 245/245. The complete library passes 865/865,
native testbench 60 passed with 1 ignored, counter 9 passed with 3 ignored, and
all-target strict clippy and formatting pass.

Step 29c extends the frontier transaction to every register-resident child,
not only the retained prefix. Before this step a multi-use moved cluster had
no VReg until its reload or recipe was emitted. The split planner therefore
selected its topology and home, but left its physical color to a later generic
allocation round. Ordinary values allocated between planning and publication
could consume the intended sparse region, and branch-exclusive children could
not share one round-wide color decision. The retained-prefix repair alone did
not make split topology and coloring one problem.

The persistent interval matrix now has allocation-round `Planned` owners.
After the source region is deferred and removed from the matrix, a reused
epoch-marked CFG projector constructs the sparse range of the retained prefix
and each multi-use child. A child definition starts at the first unused stable
sequence of its immutable insert-before-use anchor; this conservatively covers
the eventual synthetic definition while excluding older synthetic rows at the
same anchor. The session queries the same allowed-register masks and physical
unions used by ordinary allocation, reserves each selected color in those
unions, and leaves a child uncolored when no color is currently free. Planned
owners are immutable blockers rather than evictable bundles, so a later fixed
value ends the round and a later register region receives an exact split
frontier. CFG-exclusive child ranges can reserve the same physical register.

Round publication first materializes every plan, then removes symbolic
occupancy, incrementally publishes exact liveness and target facts, and maps
the resulting VRegs back to the selected colors. Exact ranges and final masks
are checked again; a changed fixed reservation or constraint leaves only the
affected fragment pending instead of installing an overlap. Tests require
planned occupancy to block an ordinary value, prohibit fact publication while
symbolic owners are live, share one color between two diamond-arm children,
cover each materialized range with its conservative symbolic range, and retain
the selected colors in both region metadata and the real matrix.

All three SIR dumps are byte-identical to Step 29b. The complete optimized and
pressure-scheduled pre-allocation MIR stages are also byte-identical, so the
generated-code delta begins at allocation. The complete non-LTO trace is at
`target/heliodor/analysis/step29d-symbolic-fragment-interval-20260718`: its MIR
falls from 196,265,123 to 194,315,959 bytes. The emitted fused
`eval_comb_apply_ff` endpoint falls from `0x3106be` to `0x2ec15e`; the latter is
within 55 bytes of the established Step 28b output. `eval_apply_ff` falls from
`0x27245c` to `0x253697`, and `eval_only_ff` from `0x1d6ceb` to `0x1b5542`.
The fused spill frame grows from `0x4000` to `0x93b0`, so this does not claim
that spill placement is solved. The dump compile took 60.819 s versus 100.486
s for Step 29b.

The full run at
`target/heliodor/results/step29d-symbolic-fragment-20260718/20260719T061321Z_celox_test_soc_linux_boot.log`
printed through `reboot: Power down` and exactly one
`cy=9ae070 x3=aa pass=1`, with 55.871 s compile and 129.836 s execute. The
independent dump and full-run compile intervals both remove roughly forty
seconds of split/reallocation work. The single execution sample is not a
repeat-qualified speed claim. Focused regalloc tests pass 247/247, the complete
library passes 867/867, native testbench passes 60 with 1 ignored, counter
passes 9 with 3 ignored, and all-target strict clippy passes.

This completes atomic coloring of already-selected register fragments; it is
not yet integrated spill placement. At this point home kind, transition
placement, and the register-region partition were still selected before all
child alternatives were visible together, and a second plan for the same
semantic root still forced a publication boundary.

Step 29d removes that forced boundary without introducing a repeated whole-
round cost scan. Different machine regions of one semantic root own disjoint
immutable root uses, so their split plans can remain private in the same
allocation-IR transaction. The round keeps one additive `RootHomePlan` cost
accumulator and one reserved-entry set per root. Evaluating a candidate extends
a copy of only that accumulator with the candidate's new entries; accepting it
adds only those entries. This lets stack creation be amortized across same-root
regions and lets a later entry change the cheapest policy for earlier entries
without concretely rewriting every prior plan after each decision.

Concrete MemorySSA, rematerialization, and stack homes are selected once when
the physical-color round closes. Publication groups all entries in one pass,
computes one exact root-wide partition, charges stack creation to exactly one
entry, and distributes the resulting homes to the deferred plans. Before any
allocation-IR mutation, an independent rebuild requires the incremental root,
entry, stack-existence, and additive-cost state to equal the deferred plans.
The mutation boundary separately rejects duplicate machine sources and
overlapping same-root use ownership. Candidate work is proportional to its new
entry count plus indexed ownership lookup; publication sorts and visits the
round entries once instead of filtering all plans once per root or reallocating
all earlier home choices after every accepted split.

The focused same-root fixture first creates two disjoint machine regions, then
forces both next entries to the implicit stack alternative. Independent plans
cost `2 + 2`; the incremental second-entry cost is `1`, the root-wide
partition costs `3`, one entry owns stack creation, both plans publish in one
transaction, and the rebuilt joint problem remains valid. Focused register-
allocator tests pass 248/248, the complete library passes 868/868, native
testbench passes 60 with 1 ignored, and counter passes 9 with 3 ignored.

This boundary did not fire on the retained Heliodor allocation. The complete
pre-optimized, post-optimized, and native-optimized SIR plus every MIR stage in
`target/heliodor/analysis/step29e-root-wide-home-round-20260718` are byte-
identical to Step 29c. The trace-only compile took 57.599 s. The optimized
non-LTO full run at
`target/heliodor/results/step29e-root-wide-home-round-20260718/20260719T064111Z_celox_test_soc_linux_boot.log`
printed through `reboot: Power down` and exactly one
`cy=9ae070 x3=aa pass=1`, with 56.025 s compile and 130.024 s execute. Because
the complete generated MIR is identical, Step 29d makes no generated-code or
execution-speed claim.

The next architectural slice must represent all child fragment topology,
physical colors, and MemorySSA/stack/rematerialization alternatives in one
allocation problem, rather than merely sharing home costs among split plans
already selected by separate pressure requests. Stores, reloads, and copies
must be published only after those joint alternatives have won.

That proposed joint alternative solver is rejected.  The free-CFG preview and
the later global-eviction experiments both made the exact accepted Heliodor
input larger than the 194,315,959-byte Step 29e MIR.  More importantly, they
kept the wrong ownership model: an earlier whole-function coloring was treated
as fixed while one failed value selected topology, color, and memory homes in
an external round.  Adding a better objective to that solver does not repair
the allocation protocol.

Step 30 replaces the protocol with the same decomposition used by LLVM's
greedy allocator.  Work is split into independently testable commits:

1. Introduce a production-used live-range state table and queue with
   `New -> Assign -> Split -> Split2 -> Spill -> Done`, plus monotonic eviction
   cascades.  Preserve the accepted output before enabling eviction or edits.
2. Move assignment ownership into one mutable sparse live-register matrix.
   A free assignment returns a physical register to the base driver.  Eviction
   unassigns cheaper victims, copies the candidate cascade to them, and
   requeues them without terminalizing their stages.
3. Replace deferred symbolic rounds with `SplitAnalysis` and `SplitEditor`.
   An edit unassigns the source, rewrites only private allocation IR, rebuilds
   exact child intervals, and returns every surviving child to the same queue.
   No child receives a hard color or a final home during the edit.
4. Require strict progress for repeated global/local splits.  A remainder may
   advance to `Spill`, but a useful child starts again at `New`; it is never a
   `NoEvict` leaf.
5. Add a spiller interface.  Stack, state-MemorySSA, and pure-rematerialization
   choices are made there at concrete insertion points.  The resulting short
   machine intervals are marked `Done` and requeued for physical assignment.
6. Remove `JointAllocationSession`, symbolic reservations, root-round home
   accumulation, and all publication outcomes from the production path.
7. Perform one final allocation-IR-to-MIR rewrite and run the independent
   assignment, stack-home, and physical-liveness verifiers.

Every commit runs its focused state-machine and interval tests followed by the
complete regalloc test group.  Every change that can affect allocation then
uses the same Heliodor input to compare complete pre/post/native SIR and MIR.
The production switch additionally runs the non-LTO Linux boot through
`cy=9ae070 x3=aa pass=1`; the final gate is a release build and kernel power
down.  Code-generation time and simulator execution time are reported
separately.  No LTO build is used during iteration.

No slice is accepted from frame size, instruction counts, compile-only output,
or a partial kernel log.  Every code-changing slice must pass the focused
verifier tests, common native tests, complete SIR/MIR inspection, and the exact
`cy=9ae070 x3=aa pass=1` Linux marker.  Release/LTO remains deferred until the
new allocator produces a substantial non-LTO execution win.

The first Step 30 checkpoint (`6251aef5`) introduced the production staged
queue, mutable matrix assignment, monotonic eviction cascades, and immediate
requeueing of edited live children.  The following checkpoint separates spill
policy from split topology: `RegionSplitPlan` no longer contains a home or
transition cost, while a function-lifetime `Spiller` owns concrete stack,
State-MemorySSA, and rematerialization decisions and their allocation-IR edits.
The symbolic-round reservation protocol has been removed, and a no-progress
range now advances through `Split2` to a concrete `Spill` obligation.

This checkpoint does not yet satisfy the whole Step 30 architecture.  A
successful partial split still materializes its moved complement immediately;
that complement must become an ordinary unmaterialized interval, return to the
queue, and reach the spiller only after its own assignment and split attempts
fail.  `JointAllocationSession` must then be removed in favor of the base
driver and final rewrite described above.

The complete trace at
`target/heliodor/analysis/step30b-spiller-separation-20260719` took 58.203 s.
Its pre-optimized, post-optimized, and native-optimized SIR and complete MIR
are byte-identical to
`target/heliodor/analysis/step28b-fixed-intervals-final-20260718`; therefore
this checkpoint makes no generated-code or execution-speed claim.  The
non-LTO full run completed through `reboot: Power down` and exactly one
`cy=9ae070 x3=aa pass=1`, with 57.321 s code generation and 124.955 s
simulation (182.285 s total).  Focused register-allocation tests pass 253/253
and the complete library passes 873/873.

Step 30c adds the machine-IR prerequisite for a real `LiveRangeEdit`.  Since
Celox keeps allocation IR in strict SSA, split boundaries are represented by
ordinary synthetic copies and pruned-IDF joins by synthetic merge phis.  Both
have real VRegs, liveness, copy/phi affinities, and final MIR rows.  The focused
diamond regression proves that incremental def/use publication equals an
independent liveness reconstruction and that atomic materialization remains
valid strict SSA.  This substrate is deliberately not yet called by the
production split planner; production output must remain unchanged until cut
placement and root-use ownership are connected as one edit.

The complete Step 30c trace at
`target/heliodor/analysis/step30c-split-ssa-ir-20260719` took 58.254 s.  All
four files are byte-identical to Step 30b and retain the same hashes, including
MIR SHA-256
`504d46eb9ea3d5a5f876b0afcdbd4d9ba664b63dddfa63b34f21cfa4cb028106`.
The non-LTO full run completed through `reboot: Power down` and exactly one
`cy=9ae070 x3=aa pass=1`, with 56.754 s code generation and 119.546 s
simulation (176.312 s total).  The complete library passes 874/874.  Because
the generated code is identical, this checkpoint makes no speed claim.

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
| 15 | point-specific MemorySSA reload costs | allocator 147/147; reload 30/30; spill plan 11/11; native MIR 6/6 | lib 759/759; native 60/60; counter 9/9; check, strict clippy, docs | CPU-0 non-LTO full run passes: `cy=9ae070 x3=aa pass=1` | compile-only 64.748 s (`execute_ns=0`); full-run compile 65.313 s; full-run execute 128.602 s | exact-use placement works; runtime effect unconfirmed; state homes and use-cluster costing remain open |
| 16 | final-use MemorySSA spill homes | allocator 149/149; native MIR 6/6 | lib 761/761; native 60/60; counter 9/9; check, strict clippy, docs | two CPU-0 non-LTO full runs pass: `cy=9ae070 x3=aa pass=1`; final-source MIR is byte-identical | inspected/final compile-only 65.240 / 65.983 s (`execute_ns=0`); full compile 64.344 / 66.136 s; execute 128.533 / 138.830 s | concrete stack traffic removed; runtime effect unconfirmed; multi-use cluster costing remains open |
| 17a | straight-line join clusters and planner-owned aggregate recipe homes | allocator 153/153; native MIR 6/6 | lib 765/765; native 60/60; counter 9/9; fixtures, check, CI-target strict clippy, docs | final-source and two CPU-0 non-LTO candidate runs pass: `cy=9ae070 x3=aa pass=1`; final MIR is byte-identical | final compile 65.618 s; final execute 136.728 s; candidate intervals remain separately recorded above | structurally complete; runtime effect unconfirmed; interval solver remains open |
| 17b | actual mixed-plan aggregate baseline | allocator 154/154; native MIR 6/6 | lib 766/766; native 60/60; counter 9/9; check and docs | CPU-0 non-LTO full run passes: `cy=9ae070 x3=aa pass=1`; complete SIR/MIR is byte-identical to Step 17a | compile 66.631 s; execute 128.829 s | cost-model correction complete; generated code unchanged; interval solver remains open |
| 17c trial | rejected (no commit) | allocator 156/156; native MIR 6/6 | trial stopped before the retained common gate | cost-only and no-home variants both pass: `cy=9ae070 x3=aa pass=1` | cost-only compile 64.069 s / execute 131.028 s; no-home compile 68.297 s / execute 141.171 s | both variants regressed and were fully reverted |
| 18 | `40e29243` | color 5/5; allocator 155/155; native MIR 6/6 | lib 767/767; native 60/60; counter 9/9; check, strict clippy, format, docs | CPU-0 non-LTO A--B--A all pass: `cy=9ae070 x3=aa pass=1`; complete SIR and spill/reload MIR bodies are unchanged | compile candidate/baseline/candidate 64.924 / 64.495 / 65.235 s; execute 127.868 / 136.098 / 128.095 s | complete; candidate mean execute -5.96%; interval solver remains open |
| 19 pre-RA reload-schedule trial | rejected (no commit) | focused reload scheduling 2/2; allocator 157/157; native MIR exposed the intended placement delta | trial reverted before retained common gate | CPU-0 non-LTO A--B--A--B--A all pass: `cy=9ae070 x3=aa pass=1`; SIR and pre-RA MIR byte-identical | candidate compile 66.116 / 64.152 / 66.008 s vs baseline 63.882 / 65.501 s; candidate execute mean 137.665 s vs baseline 131.124 s | execute +4.99%; fully reverted; post-RA scheduling remains open |
| 19 post-color reload-schedule trial | rejected (no commit) | focused physical-use/MemorySSA barriers 3/3; allocator 158/158; native MIR 6/6 | trial reverted before retained common gate | CPU-0 non-LTO A--B--A all pass: `cy=9ae070 x3=aa pass=1`; assignments and non-load instruction order byte-identical | candidate compile 69.308 / 67.862 s vs baseline 65.766 s; candidate execute mean 134.911 s vs baseline 129.176 s | execute +4.44%; fully reverted; fixed-distance scheduling closed |
| 20 first-use join-cost trial | rejected (no commit) | focused first-use regression 1/1; allocator 155/155; non-LTO runner build | trial reverted before retained common gate | CPU-0 non-LTO A--B--A all pass: `cy=9ae070 x3=aa pass=1`; SIR and pre-RA MIR byte-identical; original `bb1767` path has two extra loads and one extra store | candidate compile 69.352 / 66.218 s vs baseline 66.532 s; candidate execute mean 133.411 s vs baseline 128.487 s | execute +3.83%; fully reverted; complete block-transition pricing required |
| 21a exact block-transition extraction | `e10c9c2e` | allocator 155/155; native MIR 6/6 | format, docs, and diff checks pass | CPU-0 non-LTO pass: `cy=9ae070 x3=aa pass=1`; complete SIR and MIR byte-identical to isolated Step 18 baseline | compile 74.179 s; execute 144.439 s; no timing claim | analysis infrastructure only; remaining throughput cause not established |
| 22 word32 snapshot propagation | `278f6ecb` plus this step | width regression 1/1; MIR optimization 53/53; allocator 155/155; native MIR 6/6 | lib 768/768; docs, format, and diff checks pass | CPU-0 non-LTO pass: `cy=9ae070 x3=aa pass=1`; all SIR stages byte-identical to Step 21a | compile 70.552 s; execute 123.199 s | concrete redundant work removed; shared-predicate live range and remaining gap open |
| 23 coupled update short-circuit and dynamic StateSSA placement | this step | BranchifyMux 44/44; placement 12/12; StateSSA 8/8 | lib 774/774; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check and format pass | CPU-0 non-LTO pass: `cy=9ae070 x3=aa pass=1`; exact final SIR/MIR trace retained | compile 77.247 s; execute 120.990 s | exact local defect fixed; execute -1.79%; dominant gap remains open |
| 24 cross-phase stable-forwarding diagnostic | rejected (no commit) | exact full SIR/MIR comparison | unchanged source and pre/post SIR hashes | CPU-0 non-LTO pass: `cy=9ae070 x3=aa pass=1` | compile 82.193 s; execute 121.003 s | no improvement; switch remains disabled |
| 26a disjoint bit-range state reload homes | this step | reload 32/32; complete normalized post-RA MIR inspection | lib 776/776; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check and format pass | CPU-0 non-LTO pass: `cy=9ae070 x3=aa pass=1`; complete SIR unchanged | compile 75.665 s; execute 114.833 s | allocator prerequisite retained; one stack home removed; aggregate promotion remains open |
| 27a exact sparse live intervals | this step | live-interval construction and independent verification 5/5 | lib 781/781; check and format pass | analysis-only module is not connected to allocation; generated MIR is unchanged | n/a | stable instruction/phi-edge slots, CFG-sparse segments, and an independent liveness verifier complete |
| 27b live bundles and HomeGraph | this step | HomeGraph 6/6; legacy reload 32/32 | lib 787/787; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, strict clippy, and format pass | parent/candidate complete pre/post/native SIR and MIR byte-identical; parent exact Linux marker remains applicable | trace-only compile: parent 74.106 s, candidate 72.213 s; no timing claim | version-independent home shapes plus exact per-use MemorySSA recipes represented; production allocator unchanged |
| 27c1 sparse physical interval unions | this step | interval-union insertion/removal/interference/free-region 4/4 | lib 791/791; check, strict clippy, and format pass | diagnostic allocator structure is not connected to production MIR | n/a | per-register sparse interference matrix and independent rebuild verifier complete |
| 27c2 complete-bundle allocation policy | this step | allocation queue/eviction/recolor/home selection 4/4 | lib 795/795; check, strict clippy, and format pass | diagnostic allocator is not connected to production MIR | n/a | terminating work queue, transactional recolor, monotonic eviction, and independent plan verification complete |
| 27c3 per-use home partition | this step | allocator 5/5 including path-specific state/stack partition | lib 796/796; check, strict clippy, and format pass | diagnostic allocator is not connected to production MIR | n/a | exact shared-stack versus per-use recipe partition replaces VReg-wide fallback |
| 27c4 dominance/use-cluster region splitting | this step | allocator 8/8 including same-block, cross-block, and sibling-arm splits | lib 799/799; check, strict clippy, and format pass | diagnostic allocator is not connected to production MIR | n/a | exact-use transitions and CFG-connected sparse register children complete |
| 27c5a use-indexed HomeGraph | this step | HomeGraph 6/6; allocator 8/8 | lib 799/799; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, strict clippy, and format pass | CPU-0 diagnostic intentionally stopped after HomeGraph completion; production MIR unchanged | HomeGraph 8.128 / 6.755 / 6.611 s for the three reported large functions | nested full-analysis rebuild and quadratic home lookup removed; allocator core still fails the actual-scale gate |
| 27c5b allocation-owned sparse ranges and transactional interval unions | this step | interval union 6/6; allocator 10/10 | lib 803/803; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, strict clippy, and format pass | CPU-0 diagnostic compile-only completed; production MIR unchanged and no Linux semantic claim | compile-only 178.890 s; allocation 21.582 / 25.729 / 99.052 / 79.597 s | cloned matrix and repeated CFG resolution removed; destructive recolor and bundle-by-register scanning remain rejected |
| 27c5c staged register queries and immutable recolor planning | this step | allocator 10/10 including multi-resident recolor and atomic error rollback | lib 803/803; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, strict clippy, and format pass | CPU-0 diagnostic compile-only completed; production MIR unchanged and no Linux semantic claim | compile-only 131.863 s; allocation 26.015 / 30.538 / 55.274 / 29.845 s | failed speculative recolor removed; per-register split-graph reconstruction remains rejected |
| 27c5d bundle-owned split topology and occupancy cuts | this step | allocator 11/11 including topology reuse, same-block, cross-block, and sibling-arm splits; interval union 6/6 | lib 804/804; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and docs pass | CPU-0 diagnostic compile-only completed; production MIR unchanged and no Linux semantic claim | compile-only 129.858 s; allocation 26.500 / 31.208 / 53.007 / 25.172 s | repeated CFG/use topology construction removed; 2.005 s total improvement is insufficient, so remaining allocation-wide work stays open |
| 27c5e root-owned home choice and additive cost plan | this step | allocator 11/11 including shared-stack partition, subset/complement equality, single-use materialization, and root-plan reuse | lib 804/804; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and docs pass | CPU-0 diagnostic compile-only completed; production MIR unchanged and no Linux semantic claim | compile-only 126.211 s; allocation 24.600 / 28.062 / 51.346 / 23.031 s | repeated home partition and losing-candidate materialization removed; free-range/candidate search remains the dominant open allocator design |
| 27c5f staged conflict/cut queries and streaming region selection | this step | interval union 6/6 including exact canonical cuts; allocator 11/11 including same-block, cross-block, and sibling-arm split semantics | lib 804/804; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and docs pass | CPU-0 diagnostic compile-only completed; production MIR unchanged and no Linux semantic claim | compile-only 111.349 s; allocation 21.190 / 23.277 / 30.433 / 11.727 s | second interval-union search and fully materialized losing candidates removed; compile-only improves 14.862 s |
| post-27c conflict discovery-order trial | rejected (no commit) | interval union 6/6; allocator 11/11 | focused check/clippy passed before measurement; full candidate reverted | CPU-0 diagnostic compile-only completed; production MIR unchanged | 135.893 s vs 111.349 s retained baseline | 22.0% compile regression; fully reverted; local container-order tuning closed |
| 27d1 allocation IR and shared synthetic-value liveness | this step | allocation IR 5/5; live interval 5/5 | lib 809/809; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target strict clippy | analysis/rewrite infrastructure is disconnected from production MIR | n/a | original and synthetic machine values now share exact CFG/phi-edge liveness; explicit home placement remains next |
| 27d2 sparse all-path stack-home verification | this step | allocation IR 8/8 including both-arm, one-arm, and same-block-order home proofs | lib 812/812; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; strict clippy | stack verifier is not yet connected to `AllocationPlan` or production MIR | n/a | explicit synthetic stack operations now have a sparse reaching-store proof; home expansion remains next |
| 27d3 allocation-home expansion into machine values | this step | allocation expansion 3/3; allocation IR 8/8 including shifted instruction and phi-edge anchors | lib 815/815; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target strict clippy | expanded problem remains disconnected from production MIR | n/a | stack/state/remat/register transitions now have exact synthetic ranges; joint reallocation remains next |
| 27d4 joint original/synthetic allocation boundary | this step | joint allocation 4/4 including affinity override, sibling-arm sharing, split requests, and fixed-pressure rejection | lib 819/819; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target strict clippy | split requests are not yet resolved and production MIR is unchanged | n/a | old assignments are affinities only; every machine range is jointly colored or returned in an exact split obligation |
| 27d5 exact pressure-region splitting and joint fixed point | this step | allocation split 5/5 including complete synthetic-pressure allocation, sibling-arm isolation, loop reentry, partial stack residency, and repeated-entry termination | lib 824/824; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and docs pass | exact result is not yet lowered to production MIR; no Linux semantic claim | n/a | reachable suffixes become multiple dominance regions or exact homes; dead synthetic DAGs are compacted and every value re-enters joint allocation |
| 27d6 atomic MIR and out-of-SSA location lowering | this step | allocation lowering 3/3 including exact stack/recipe lowering, stale-input rejection, and more-than-K phi rows | lib 827/827 under `interval-diagnostic`; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and docs pass | production MIR is unchanged; no Linux semantic or timing claim | n/a | the closed joint result lowers atomically; exact stack/immediate phi locations avoid fictitious source and destination pressure; constraints, coalescing, and stack coloring remain next |
| 27d7a split target constraints and weighted coalescing | this step | allocation constraints 2/2; pre-spill Perm pressure regression 1/1; joint allocation 4/4; split 5/5; lowering 3/3 | lib 830/830 under `interval-diagnostic`; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and docs pass | production MIR is unchanged; no Linux semantic or timing claim | n/a | fixed/clobber boundaries split SSA ranges before allocation; rewritten operands own mandatory masks; sparse transactional coalescing preserves correctness; stack-home lifetime coloring remains next |
| 27d7b exact stack-home liveness and sparse slot coloring | this step | stack coloring 3/3 for exact reuse/interference/direct phi edges; allocation lowering 3/3; more-than-K phi-edge regression 1/1 | lib 833/833 under `interval-diagnostic`; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and docs pass | production MIR is unchanged; no Linux semantic or timing claim | n/a | stores/stack phis define location SSA; reload/direct-edge uses share the CFG-sparse verifier; a dynamic interval matrix colors and independently rebuilds final frame slots |
| 27d8 explicit replacement publication and actual-scale rejection | this step | destination-qualified final-liveness regression 1/1; candidate regalloc 12/12 including 32-phi JIT execution | candidate/default lib 834/834; candidate native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored | no result: non-LTO candidate remained in code generation and timed out before Linux execution | 900.256 s timeout; compile/execute record unavailable | explicit publication is verified on common workloads, but the all-world split/reanalyse/recolor fixed point is rejected; persistent incremental allocation is next |
| 27d9a stable block slots and allocation-session identities | this step | stable cross-block slot 1/1; stable dead-materialization identity 1/1; split 5/5; lowering 3/3 | candidate lib 836/836; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check and all-target strict clippy pass | not rerun; this prerequisite does not yet replace whole-problem liveness/recoloring | n/a | block-local coordinates and monotonic synthetic IDs remove global renumbering; persistent region/matrix state remains next |
| 27d9b persistent physical interval allocation | this step | stable bundle-hole 1/1; retained matrix-membership 1/1; joint allocation 6/6; split 5/5 | candidate lib 838/838; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check and all-target strict clippy pass | no result: compile-only candidate timed out with three joint-allocation workers active | 600.292 s timeout; HomeGraph/root/expand phases complete | unchanged values retain matrix membership, but repeated whole-IR liveness remains dominant and is the next architectural boundary |
| 27d9c differential allocation liveness | this step | incremental instruction/phi-edge liveness 2/2; regalloc 221/221; split 5/5 | candidate lib 840/840; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored | not rerun; global constraint/region/joint-row rebuilding remains in the split loop | n/a | changed block facts rebuild only affected sparse SSA ranges; independent whole-program proofs remain at publication; session-owned constraints and semantic rows are next |
| 27d9d differential target constraints | this step | incremental clobber/phi-affinity constraints 2/2; regalloc 223/223; split 5/5 | candidate lib 842/842; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored | not rerun; global joint region/value rebuilding and full transactional clones remain | n/a | fixed/clobber/copy/phi facts are block indexed and masks update only changed VRegs; complete constraints are independently rebuilt at publication |
| 27d9e persistent semantic rows and in-place split session | `ae170fbc` | differential/full joint identity in partial-stack split; differential/full DCE identity; regalloc 223/223; split 5/5 | candidate lib 842/842; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored | no result: actual-scale compile stopped on stale cross-block region liveness | 217.609 s; execute unavailable | stable ownership/definition/region indexes update changed rows only; actual scale exposed an incomplete split mutation set |
| 27d9f unified split mutation journal | `a8b8d2cb` | cross-block register-region differential/full identity; regalloc 224/224; split 6/6 | candidate lib 843/843; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored | no result: two functions published, then fixed-only joint pressure stopped compilation | 266.162 s; execute unavailable | all operand rewrites update liveness and constraints from one transaction journal; fixed transition production/coloring remained open |
| 27d9g semantic-only phi identities and canonical edge locations | `fa0ed954` | unused-phi physical liveness; semantic-only assignment/SSA-destruction boundaries; regalloc 228/228; SSA destruction 21/21 | candidate lib 848/848; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, and format pass | CPU-0 non-LTO full run passes: `cy=9ae070 x3=aa pass=1` | compile-only 456.876 s; full compile 442.109 s; execute 267.183 s | fixed-only false pressure removed and all units publish; runtime is 2.33x Step 26a, exposing whole-live-set constraint permutations as the next rejected design |
| 28a local fixed-use fragments without whole-live-set permutations | this step | fixed-operand isolation and clobber non-mutation; legalize 10/10; regalloc 230/230 | candidate lib 850/850; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, and format pass | CPU-0 non-LTO full run passes: `cy=9ae070 x3=aa pass=1` | compile-only 384.401 s; full compile 382.284 s; execute 124.495 s | effective comb edge copies fall 98,037→1,334 and execute improves 53.4%; exact fixed-register interval reservations remain next |
| 28b immutable fixed intervals and exact pressure cuts | this step | live interval 9/9; interval union 7/7; constraints 4/4; joint allocation 7/7; split 6/6; regalloc 232/232 | candidate lib 852/852; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, and format pass | three CPU-0 non-LTO full runs pass: `cy=9ae070 x3=aa pass=1`; complete Step 28a/final-28b SIR, MIR, assignments, and disassembly are byte-identical | adjacent compile-only 53.619 s (28a) / 52.540 s (28b); final-source compile 56.282 s; execute 141.988 / 118.501 / final 122.963 s | clobbers are exact immutable `[barrier, def)` occupancy and split requests carry owner-qualified cuts; no generated-code or speed claim; integrated fragment allocation/spill placement remains next |
| 28c stable allocation-session deltas | this step | stable original/synthetic coordinates; exact block-fact diff; affinity/reservation revisions; regalloc 239/239 | interval lib 859/859; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, format, and diff checks pass | non-LTO full run passes once through kernel power-down with exactly one `cy=9ae070 x3=aa pass=1` | compile-only 163.886 s; full compile 163.766 s; execute 149.306 s | rejected updater compile time reduced 29.7%; still far above committed Step 28b and 5--7 GiB RSS, so persistent interval/session indexing remains open |
| 28d block-transaction allocation publication | this step | exact producer journal vs changed-block oracle; staged dense-row publication; anchor-local stable sequences; shared immutable use rows; regalloc 240/240 | interval lib 860/860; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and diff checks pass | non-LTO full run passes through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1` | final full compile 56.294 s; execute 127.017 s | per-insertion dense shifts and duplicate fact/use-row reconstruction removed; compile interval returns to the Step 28b range; integrated fragment allocation remains next |
| 29a register-specific multi-cut frontiers | this step | interval suffix query; two-arm free-prefix frontier; one-transaction multi-cut split; regalloc 244/244 | interval lib 864/864; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target strict clippy, format, and complete IR dump pass | optimized non-LTO `interval` run passes through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1` | compile-only 98.538 s; full compile 94.492 s; execute 138.064 s | joint allocator completes at scale without mixing colors; no execution-speed claim; integrated multi-fragment spill placement remains open |
| 29b retained-fragment color ownership | this step | retained original/region affinity; rebuilt-row/matrix assignment; occupied-color fallback; regalloc 245/245 | interval lib 865/865; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target strict clippy, format, complete IR dump, and SIR identity checks pass | optimized non-LTO `interval` run passes through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1` | dump compile 100.486 s; full compile 95.104 s; execute 133.710 s | selected frontier color now survives split publication and shortens both large emitted bodies; integrated symbolic fragment/home selection remains open |
| 29c symbolic child-fragment coloring | this step | planned matrix occupancy; round-boundary blocking; disjoint child-color reuse; conservative-to-exact range/color transfer; regalloc 247/247 | interval lib 867/867; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target strict clippy, format, complete IR dump, and pre-RA identity checks pass | optimized non-LTO `interval` run passes through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1` | dump compile 60.819 s; full compile 55.871 s; execute 129.836 s | every selected register child participates in the current physical matrix before its VReg exists; integrated MemorySSA/home selection remains open |
| 29d root-wide deferred home partition | this step | same-root disjoint ownership; incremental home-cost identity; shared stack creation; one-transaction publication; regalloc 248/248 | interval lib 868/868; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target strict clippy, format, complete IR dump, and exact MIR identity checks pass | optimized non-LTO `interval` run passes through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1` | dump compile 57.599 s; full compile 56.025 s; execute 130.024 s | duplicate-root publication boundary removed with incremental root costs; Heliodor MIR unchanged; joint topology/color/home alternatives remain open |
| 30a greedy live-range driver | `6251aef5` | staged queue, eviction cascade, split-child requeue, and matrix ownership regressions | lib 875/875; non-LTO build, format, and diff checks pass | complete SIR/MIR byte-identical to Step 28b | trace-only compile recorded separately; no timing claim | production allocation follows staged greedy control flow; legacy symbolic spill ownership remains for removal |
| 30b dedicated spiller boundary | `ce398251` | regalloc 253/253 including concrete Spill-stage and topology-only child regressions | lib 873/873; non-LTO build, format, and diff checks pass | pass through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1`; complete SIR/MIR byte-identical to Step 28b | trace 58.203 s; full code generation 57.321 s; simulation 124.955 s | split topology and spill policy are separated; partial spill remainder and base-driver replacement remain open |
| 30c strict-SSA live-range-edit substrate | this step | split-copy/merge-phi incremental-versus-full liveness and atomic-materialization regression | lib 874/874; non-LTO build and allocation-IR 12/12 pass | pass through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1`; complete SIR/MIR byte-identical to Step 30b | trace 58.254 s; full code generation 56.754 s; simulation 119.546 s | real copy and merge-phi values are representable; production cut editing and child ownership remain open |

## Related design records

- [Simulator architecture](./architecture.md)
- [Native register allocation](./native-register-allocation.md)
- [Branch-aware mux lowering](./branch-aware-mux-lowering.md)
- [JIT roadmap](./jit-roadmap.md)
- [Heliodor macro benchmark](../benchmarks/heliodor.md)
