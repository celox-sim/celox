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

At the Step 30b checkpoint, a successful partial split still materialized its
moved complement immediately.  Steps 30c--30f replace that operation with
ordinary strict-SSA machine intervals returned to the queue.  The remaining
architectural cleanup was removal of `JointAllocationSession` and the obsolete
home-producing planner from the production driver in favor of the base driver
and final rewrite described above. Step 30g completes that cleanup; the old
planner remains test-only historical coverage.

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

Step 30d implements a strict-SSA `SplitEditor` transaction, still disconnected
from the production split request.  Each exact frontier cut is projected onto
the latest legal stable machine boundary, where a real copy consumes the old
representative and defines a new one before the cut.  The editor places merge
phis over the pruned iterated dominance frontier and renames both semantic
uses and copy inputs along the dominator tree.  A loop regression specifically
requires a backedge copy to create a header phi and feed the next iteration;
the diamond regression requires independent arm copies and one join phi.
Incremental liveness must exactly equal an independent full reconstruction in
both cases, and final private-IR materialization must remain valid strict SSA.

This checkpoint does not yet alter production allocation or claim a generated-
code/runtime change.  The next slice is not another split heuristic: it must
make the returned representatives own exact root uses and transition uses,
requeue their exact intervals, and leave the complement unmaterialized until
that interval itself reaches the `Spill` stage.

Step 30e connects SplitEditor output to allocation ownership while keeping the
production selector on the preceding path.  Exact `LiveInterval` machine uses
are canonical; immutable HomeGraph use IDs are only annotations for
HDL-specific home pricing.  Source, copy, and merge representatives receive
stable metadata and ordinary spillable `Region` rows even when their direct
semantic-use subset is empty.  A focused two-arm edit rebuilds the complete
joint problem independently and requires all four representatives to have
exact sparse intervals and spill costs, while only the merge representative
owns the downstream logical-root use.

Step 30f completes the production switch.  Split selection now reads physical
free-prefix topology only, probes legal strict-SSA copy boundaries, and calls
`LiveRangeEdit`; it neither queries HomeGraph costs nor materializes the
remainder.  Source, copy, and pruned-IDF merge products return as exact queue
units with no hard color.  A same stable-anchor-zone guard rejects repeated
copy-only non-progress without imposing an arbitrary iteration cap.

Generic machine spilling is now the `Spill`-stage fallback for representatives
whose exact uses extend beyond direct semantic root uses.  It can store an
instruction definition, assign a stack destination to an original or
synthetic phi, insert reloads immediately before older synthetic copies/phis,
and remove only the exhausted representative.  Reload products are ordinary
short machine values and final lowering sees the same stack-home facts.  The
focused production regression executes split, requeue, machine spill, final
allocation lowering, and MIR verification as one transaction.

The complete non-LTO trace at
`target/heliodor/analysis/step30f-production-split-editor-20260719` took
53.926 s and contains all 58,353,245-byte pre-optimized SIR,
19,713,339-byte post-optimized SIR, 20,313,891-byte native-optimized SIR, and
194,315,959-byte MIR outputs.  They are byte-identical to the accepted Step 29e
input/output artifacts; the MIR SHA-256 is
`a2c7746d55bf4bbdbf1454763177dd055694db8cbf058584174924e83b123741`.
The non-LTO O2 Linux run in
`target/heliodor/results/20260719T110345Z_celox_test_soc_linux_boot.log`
completed through `reboot: Power down` and exactly one
`cy=9ae070 x3=aa pass=1`: code generation took 52.491 s and simulation took
116.325 s (168.826 s total).  This single run proves semantics for the switch;
because generated MIR is unchanged and runtime is noisy, it makes no speed
claim.

Step 30g does more than rename `JointAllocationSession`: it extracts its mixed
state into conventional owners. `MachineLiveIntervals` owns exact intervals,
incremental constraints, and
semantic-use annotations; `LiveRegMatrix` separately owns sparse range tokens,
occupancy, and assignments; `GreedyAllocator` coordinates those owners and
directly owns the staged worklist, eviction protocol, and selection scratch.
Home plans stay in `Spiller` and are passed into interval-cost refreshes
without being retained by either
allocation owner.  Production constructs that spiller directly, so the legacy
home-producing split context's dominance and per-root use-topology analyses
are no longer built on the production path.

The complete non-LTO trace at
`target/heliodor/analysis/step30g-greedy-owners-20260719` took 56.072 s.  All
four full outputs are byte-identical to Step 30f, including MIR SHA-256
`a2c7746d55bf4bbdbf1454763177dd055694db8cbf058584174924e83b123741`.
This is therefore exactly the generated code already observed through
`reboot: Power down` and `cy=9ae070 x3=aa pass=1`; a second simulation cannot
add a code-semantic distinction.  The single trace timing is noisy and makes
no code-generation speed claim.

The final release/LTO run at
`target/heliodor/results/20260719T113800Z_celox_test_soc_linux_boot.log` also
completed through `reboot: Power down` and exactly one
`cy=9ae070 x3=aa pass=1`.  Code generation took 51.122 s and simulation took
131.346 s (182.479 s reported total).  This is a qualification run, not a
speed comparison: it is one noisy sample and the generated MIR is unchanged.

### Step 31: Replace layer lowering with pressure-scheduled dependency regions

The source scheduler currently assigns every acyclic logic path a longest-path
layer, gathers all paths in that layer, and materializes grouped results before
their stores.  Equal layer says only that the paths are unordered.  It says
nothing about a profitable order, and a wide layer keeps unrelated results and
shared lowering-cache entries live until the whole layer is flushed.  Renaming
that layer to a ready frontier, target transaction, or output cone would retain
the same defect if the complete group were still materialized at once.

This step removes the layer as a lowering unit.  A maximal acyclic run between
loop SCCs and explicit observable-order barriers is a *scheduling region*, not
a batch.  Individual logic paths remain the scheduling nodes.  The scheduler
works bottom-up from region exits, exactly as the native MIR pressure scheduler:

1. data edges are register-value uses; explicit order edges are hard
   dependencies but do not invent a live value;
2. selecting a reverse-ready path removes its result if that result is live and
   makes each not-yet-scheduled data input live;
3. below the x86-64 allocatable capacity, dependency depth selects work which
   preserves instruction-level parallelism; a candidate which would exceed
   that capacity loses to the smallest live-pressure delta;
4. indexed ready buckets are updated only for paths incident to a value whose
   liveness changed; no selection scans the complete ready set; and
5. lowering follows the resulting forward order.  A store or event is emitted
   when its path is lowered.  Cross-region SLT cache entries are not retained
   merely because two paths happened to share a former layer.

This order makes every single-use producer/consumer chain contiguous.  Values
at a real fanout boundary can still have a long range; that is an actual
multi-use value and is left to live-range splitting, rematerialization, and the
spiller rather than hidden by a source-order heuristic.  Exact multi-output
folds remain atomic, but their packed result is retained while projections are
consumed and stored one at a time.  Optional store coalescing may combine only
consecutive paths whose projected temporary pressure fits the same physical
capacity; it cannot turn a complete ready frontier back into one batch.

For `N` logic paths and `E` deduplicated dependency/order edges, SCC analysis
and region construction use `O(N + E)` time and space.  Indexed bottom-up list
scheduling uses `O((N + E) log N)` time and `O(N + E)` space: every path enters
and leaves a ready bucket once, and every edge causes at most one liveness or
priority update.  The implementation must not retain a path-by-cone reachability
set, clone a graph per output, construct a dense candidate matrix, or use an
all-pairs cone merge.  A 4,096-node independent-region regression records queue
work and rejects a shrinking-ready-set scan even if wall-clock timing happens
to pass.

The implementation is split at correctness boundaries:

1. add the path scheduler and its dependency-order, fanout, order-only, wide-
   value, and linear-work regressions without changing production lowering;
2. replace `layer`, `reorder_dag_runs`, and `pending_layer` with the scheduled
   order, then run focused scheduler/observer/false-loop tests and the complete
   non-LTO `celox` suite;
3. make cache lifetime and exact-fold projection consumption agree with the
   scheduled regions, verify complete SIR/MIR, and run the non-LTO Linux boot
   through `cy=9ae070 x3=aa pass=1`; and
4. compare code generation and execution intervals against the accepted
   Step 30 input.  No compile-only, frame-size, or partial-kernel result accepts
   the step, and release/LTO remains a final gate only.

The production implementation deliberately stops one level below the proposed
source pressure model.  A `LogicPath` target width is not the number or shape
of machine temporaries produced while lowering its SLT expression, so using it
as register pressure produced a valid path order but a worse Heliodor
allocation.  That scheduler and its bounded-work regressions remain test-only.
Production instead drains the SCC condensation graph as a deterministic stream
of individually ready components.  An effect domain is only a ready-queue
preference; it never contracts a ready frontier into a lowering batch and
never pulls a path across a dependency.  Ordinary paths are lowered and stored
immediately.  Exact grouped folds alone use a fixed 16-root window: the packed
result is computed once, then each projection is created immediately before
its store rather than keeping every projection live.

The real machine scheduling unit is the MIR instruction DAG inside each legal
basic-block region.  Its memory dependence builder now consumes shared MIR
read/write effects and uses a sparse interval partition.  RAW, WAR, and WAW
edges are exact for known ranges, while unknown aliases remain conservative;
space is proportional to effect endpoints rather than the byte length of a
sparse RTL state region.  Moving large commit/worklist pseudos into the same
region was separately evaluated and not retained: it worsened all affected
Heliodor spill frames.  Those pseudos remain barriers until their machine
expansion and pressure are represented directly.

An additional trial gave every cross-block constant one block-local
`LoadImm`.  It reduced `eval_comb`'s spill frame from 31,040 to 30,096 bytes but
grew MIR and executed an immediate materialization in every using block.  The
normal Linux test still reached the exact marker, but execution took 122.119 s
against the accepted Step 30f 116.325 s sample.  The trial was removed.  This
confirms that constant splitting/rematerialization belongs in operand
selection and the allocator's use-cluster decision, not an unconditional MIR
rewrite.

The retained complete non-LTO trace is
`target/heliodor/analysis/step31-effect-stream-range-core-v1-20260719`.  It
contains 58,885,451-byte pre-optimized SIR, 19,645,576-byte post-optimized SIR,
19,951,730-byte native-optimized SIR, and 196,224,214-byte complete MIR.  Spill
frames are 31,032 bytes for `eval_comb`, 0 for `apply_ff`, 7,288 for
`eval_apply_ff`, 7,024 for `eval_only_ff`, and 38,152 for the fused function.
Focused parser, memory-effect, and MIR-scheduler tests pass 18/18, 6/6, and
16/16.  The complete library passes 891/891, native testbench passes 60 with 1
ignored, and counter passes 9 with 3 ignored.

The matching ordinary, trace-free testbench run reached `reboot: Power down`
and exactly one `cy=9ae070 x3=aa pass=1`.  Code generation took 57.777 s and
execution took 133.430 s.  This is a correctness and complexity checkpoint,
not a speed claim: it removes the invalid layer-lowering architecture but does
not close the aggregate-memory execution gap identified in Step 25.  The next
throughput boundary remains Step 26/27e: range StateSSA with lazy packed
writeback must expose its state/rematerialization/stack alternatives to the
greedy allocator before eliminating broad memory round trips.

Status: **complete as the no-layer lowering checkpoint; no throughput gain is
claimed**.

### Step 32: Connect range StateSSA to allocation-owned lazy writeback

Step 25 identified overlapping aggregate state round trips, not stack traffic
alone, as the dominant remaining generated-work difference.  The failed Step
26 trials also established the ordering constraint: replacing loads before
packed state is an optional allocator home creates long register/stack live
ranges, while deleting stores before writeback placement loses that home.  The
next implementation therefore exposes range versions and home alternatives
before changing executable SIR.

Step 32a adds a disconnected sparse range-StateSSA analysis.  For each eligible
static two-state object it sweeps all access endpoints into non-overlapping
atoms; a differently shaped overlap is now a use/definition of the same atoms,
not a kill.  Each store records the exact source projection defining every
atom, and each load records the atom versions and destination offsets required
to compose its value.  Dynamic/element accesses, commits with unresolved phase
semantics, eventful stores, four-state storage, and externally rejected aliases
reject the complete object rather than permitting a partial rewrite.

Pruned liveness uses one shared worklist of actual `(atom, block)` pairs, and
phi placement visits only the corresponding dominance-frontier pairs.  There
is no per-byte table, independent whole-CFG traversal per atom, or dense
`atoms * blocks` matrix.  For `A` accesses, `P` endpoint atoms, and `L` sparse
live pairs, construction uses `O(A log A + P + L + incident CFG/DF edges)` time
and `O(A + P + L)` storage.  An independent verifier rebuilds reaching
versions along the dominator tree, predecessor phi inputs, and exact
load/store coverage from the published rows.

Focused tests pass 8/8 for mixed-width composition, two independently defined
diamond atoms, a loop phi only for the backedge-written atom, object-local
dynamic rejection, event/four-state rejection, and a 4,096-access sparse
object whose storage remains proportional to endpoints rather than address
span.  Terminal state visibility is now an explicit sparse use: forward
propagation visits only dirty `(atom, block)` pairs, adds liveness only at
reachable terminal blocks, and consequently places required join/loop phis
even when no ordinary SIR `Load` follows a store.  The verifier independently
reconstructs both the reachable dirty boundary and its reaching versions.

Step 32b builds the disconnected allocation-facing residency graph.  Every
load fragment is one optional packed-state use and every terminal boundary is
one mandatory packed-state use of its exact range version.  A version offers
pre-existing state, deferred writeback, or phi-inherited state as applicable;
it is never promoted as one mandatory whole-function register range.  Phi
inheritance is condensed by an iterative SCC analysis.  Internal loop edges
preserve an already established state home, while every external incoming edge
remains an explicit dependency, so a self-cycle cannot vacuously prove that
state is current.

The graph does not enumerate possible use subsets.  Instead the allocator may
submit one selected use cluster, for which the planner computes a writeback at
the deepest common dominator, before the first same-block use or at the block
exit.  Splitting that set naturally yields independent branch-local homes;
keeping it together yields one shared writeback.  The verifier checks the
definition/order/dominance proof for every concrete cluster.  Construction is
`O(V + U + E)` time and storage for range versions, actual uses, and phi inputs,
in addition to the sparse StateSSA facts; there is no candidate-cluster power
set or atom-by-block matrix.

Focused range tests pass 14/14, including load-free terminal phis, path-local
dirty exits, straight-line shared homes, branch-local versus shared clusters,
diamond inheritance, loop self-edge condensation, cross-version rejection,
and verifier corruption.  The complete optimized non-LTO library passes
905/905, native testbench passes 60 with 1 ignored, counter passes 9 with 3
ignored, and check, strict all-target clippy, format, and diff gates pass.  Both
analyses remain disconnected from executable SIR, so this slice deliberately
has no generated-code or Linux timing claim.

Step 32c gives allocation IR explicit `StateStore` and `StateReload`
operations.  Their home is one full machine-accessible 8/16/32/64-bit packed
state word identified by physical SimState offset and a versioned home ID.  It
is deliberately not an arbitrary HDL width attached to a MIR VReg.  A selected
definition therefore ends at one real state store, and each later use begins
at one real state reload; both short machine ranges re-enter ordinary exact
liveness, interference, and coloring.  Materialization emits the corresponding
SimState MIR store/load, and effect-aware synthetic DCE retains stores while
allowing dead reloads to disappear.

Atomic allocation lowering now independently verifies every such reload before
publishing MIR.  The verifier scans only bytes belonging to requested homes,
intersects known original writes through the shared MIR memory-effect model,
and rejects an unknown direct SimState alias.  It then constructs pruned sparse
byte MemorySSA, performs dominator-tree renaming, and resolves loop-carried phi
cycles with iterative SCC condensation.  A reload is legal only when every
physical byte and every reaching CFG path resolves to its exact home ID;
overlapping state homes and ordinary MIR stores invalidate only intersecting
bytes.  No byte-by-block matrix or reload-path enumeration is built.  For `I`
allocation instructions, `R` requested physical bytes, `K` intersecting
write-byte facts, `L` live `(byte, block)` pairs, and `E` MemorySSA edges, the
verification uses `O(I log R + K + L + E)` time and `O(R + K + L + E)` extra
storage.

Focused state-home tests pass 8/8 for exact materialization and short reload
liveness, every-arm and missing-arm diamonds, overlapping and disjoint
synthetic/original writes, loop-carried entry homes, and conflicting home
identity.  Allocation-IR tests pass 21/21.  The complete non-LTO library passes
913/913, native testbench passes 60 with 1 ignored, counter passes 9 with 3
ignored, and package all-target strict clippy, check, format, and diff gates
pass.  No production range plan emits these operations yet, so this slice has
no Linux semantic or timing claim.

The next slice maps range versions to full machine-word roots and publishes one
allocator-selected use cluster as state operations plus exact use rewrites.
Phi-inherited homes must remain edge facts until the complete atomic allocation
rewrite can represent them; they must not be converted back into eager broad
loads or long arbitrary-width register ranges.

Step 32d connects full machine-word versions to the production SSA allocator
and establishes the final proof boundary.  Reconstruction records each
allocator-owned state store by its final per-block SimState-write identity and
each reload by its strict-SSA destination.  Final MIR is converted back to the
allocation IR, only those exact operations are tagged as allocator-owned, and
the sparse physical-byte MemorySSA verifier proves that every reload is reached
by the selected home on every byte and CFG path.  Ordinary state recipes use a
stable original-write identity: allocator-inserted writes no longer renumber a
later disjoint original write, but an inserted write which actually reaches
the recipe remains a distinct version and is rejected.  This removes the
incorrect dependency on probe-MIR write ordinals without weakening alias or
path checks.

The production integration experiment also exposes why eager physical-word
promotion is the wrong allocation boundary.  It creates one VReg for every
non-entry cell version and makes terminal visibility a use of that VReg before
the ordinary spill planner sees the function.  A direct store which previously
ended a range can therefore become a value live to a terminal writeback; a
short direct load can become a cross-block phi range.  Deferred state homes
recover some spills only after those ranges and their pressure already exist.
On Heliodor this leaves `eval_comb` with a 46,120-byte frame versus the accepted
31,032-byte baseline, `eval_only_ff` with 13,168 versus 7,024 bytes, and even
turns `apply_ff`'s zero-byte frame into 1,576 bytes.  The complete generated MIR
is retained at
`target/heliodor/analysis/step32g-stable-write-identity-20260719`.

All 924 optimized non-LTO library tests pass, along with 60 native-testbench
tests (1 ignored), 9 counter tests (3 ignored), and the focused final-write
identity/overlap regressions.  The unmodified Heliodor checkout reaches
`reboot: Power down` and exactly one `cy=9ae070 x3=aa pass=1`.  Trace-free code
generation takes 86.870 s and execution takes 128.011 s, compared with the
accepted Step 30f 52.491 s and 116.325 s.  Correctness is established, but the
eager promotion experiment is rejected as a performance design.

The replacement unit is a MemorySSA def-use cluster, not a scheduler layer and
not a whole physical cell.  Original loads and stores remain memory operations
unless allocation selects a concrete cluster.  A selected store-to-load
cluster introduces only the short register regions needed between that
definition and those uses; branch-local clusters split independently, and
terminal writeback remains a memory obligation rather than an artificial
whole-CFG register use.  Stack, existing-state, deferred-state, rematerialized,
and register alternatives are then priced and allocated together before the
atomic MIR rewrite.

Step 32e extracts the structure-independent compiler analyses into the new
`celox-analysis` crate.  The shared crate now owns checked iterative CFG
construction, stable RPO, Lengauer--Tarjan dominators, postdominators,
frontiers, control dependence, SCC/loop facts, generic pruned SSA, sparse
MemorySSA, and interval-based memory-dependence tracking.  The SIR and MIR
callers are adapters over that substrate rather than owners of private graph
algorithms.

The first state-forwarding migration initially passed every ordinary focused
test but had an unacceptable memory design.  First, `state_promote` fed all
MIR read effects into MemorySSA queries, so a large `SparseCommit` source range
became one SSA use per byte.  Restricting queries did not fix the design: the
first shared MemorySSA still represented every tracked byte as an independent
SSA variable, expanded every broad exact write over all overlapping tracked
bytes, and ran pruned liveness over `(byte, CFG block)` pairs.  Its worst-case
storage was therefore proportional to `tracked bytes * CFG blocks`, and a
wide write could add `tracked bytes` definitions.  Running that implementation
on the Linux workload exhausted WSL memory.  This was an implementation error,
not a build-tool or rust-analyzer problem.

The retained replacement is access-based MemorySSA in the conventional
MemoryDef/MemoryPhi form, but its abstraction boundary is narrower than the
first replacement. `MemoryAccessGraph<D>` owns only live-on-entry,
MemoryDef/MemoryPhi nodes, and defining edges. `MemoryPointMap<P>` separately
maps caller-selected instruction and block boundaries into that graph.
`AliasOracle<D, Q>` and `ClobberWalker` consume caller-owned definition effects
and query objects; byte ranges are one implementation in the independent
`memory` module rather than fields required by MemorySSA. Read-result tables,
MIR coordinates, value numbers, reload fragments, and query scratch are not
stored in the shared graph.

For `B/E` CFG blocks/edges, `C` captured program points, `D` writing
definitions, and `F` MemoryPhi inputs, graph plus point-map storage is
`O(B + E + C + D + F)` and is independent of every effect's byte length. One
`ClobberQuery` owns reusable `O(D + F)` scratch. All starting points for the
same alias query share its resolved accesses, so capturing a root and its
reachable Phi inputs visits each access at most once rather than restarting a
whole-graph walk per Phi edge. Across `Q` distinct alias queries the
conservative worst case remains `O(Q * (D + F))`, excluding the caller's alias
test. Regressions cover a 16 MiB range represented by one MemoryDef and a
custom non-byte alias domain using the same graph and walker.

The MIR adapter also stopped cloning and fully reanalyzing its CFG. SSA now
accepts a minimal CFG view, and `NormalizedCfg` supplies its existing
predecessors, successors, dominator children, and frontier directly.  MIR CFG
normalization uses a forward-only shared analysis, so it no longer constructs
and immediately discards postdominators, control-dependence tables, and SCC
membership.

State promotion is now a client adapter: it owns MIR write effects and converts
clobber accesses into its private reaching-definition result. Reload planning
and lowering use a different adapter. One SimState-writing MIR instruction is
one shared MemoryDef; exact instruction-before/after and block-entry/exit
accesses produce the point-specific snapshot used by a selected recipe. A
`MemorySnapshot` records its stable root clobber and every reachable,
query-specific Phi equation. Final reconstruction therefore compares the
actual clobber graph, including Phi inputs, while translating allocator-owned
store ordinals back to the stable original-write domain. Merely retaining the
same `Phi(block)` no longer hides a changed incoming write. The old reload
`MemoryVariable::Byte`, private phi placement, affected-byte expansion,
dominator rename state, and phi-SCC canonicalizer have all been removed;
store-home and bit-fragment preservation remain deliberately client-local.

The first current-revision Linux gate exposed one missing abstraction at this
boundary. Shared edge-reload reconstruction may factor several predecessor
edges through a new write-free block. Access-based MemorySSA then represents
the same reaching writes with one additional MemoryPhi, while the original
snapshot names the predecessor writes directly. Comparing the two graph
shapes literally rejected this semantics-preserving CFG factoring before JIT
execution. Reconstruction now emits a `MemoryPhiFactoring` proof record for
each block it creates. The final verifier checks that record against the final
CFG's exact predecessor set and unique successor and independently rejects any
instruction in the block which may write SimState. Only a factoring which
passes those checks is inlined during snapshot comparison; an unrecorded Phi
or any changed incoming write remains a mismatch.

Current bounded validation is: `celox-analysis` 32/32, access-based state
promotion 9/9, snapshot-based reload planning and final verification 35/35,
optimized non-LTO library 917/917, native testbench 60 passed with one upstream
ignore, and counter 9 passed with three Veryl ignores. Package check, format,
and diff checks pass. A focused final-verifier regression changes one input of
an otherwise identically named MemoryPhi and confirms that the structural
snapshot rejects it. A second regression accepts the same writes factored only
through recorded write-free CFG structure, rejects that structure without its
proof record, and rejects a changed write beneath the recorded factoring.

After fetching Heliodor revision `7ad830fc0f8506c934b61a853ce2eadfa5926b82`,
the current revision completed the full optimized, non-LTO Linux workload in
`20260720T020745Z_celox_test_soc_linux_boot.log`. Code generation took
164.189 s, execution took 126.644 s, and the 48-line log ended with
`reboot: Power down`, `cy=9ae070 x3=aa pass=1`, and an explicit pass result.
The cycle count is unchanged from the preceding accepted run; the increased
code-generation time and remaining native-execution gap are performance work,
not an unverified correctness claim.

Status: **32a--32d correctness boundary complete; eager whole-version
promotion rejected; shared analysis extraction and both reaching-def/snapshot
adapters complete; current Linux acceptance complete; use-cluster allocation
and throughput work are in progress**.

### Step 33: Lower SLT from source MemorySSA regions

Step 31 removed `layer` as the production lowering batch, but its retained
effect-domain stream still chose among ready paths without representing which
memory definition supplies each input. That is insufficient: it can satisfy
topological order while separating a single-use producer from its consumer,
and the later SIR pass then sees unnecessarily long Store-to-Load ranges.

SLT does not yet have a control-flow graph on which to construct ordinary
MemoryPhi nodes. Its acyclic portion does, however, already have the
single-definition form of MemorySSA. Every variable LogicPath target is one
`MemoryDef` of an exact `(object, bit interval)`. A normal source is a
`MemoryUse` of every overlapping definition, while uncovered bits are
live-on-entry. A previous-value source reads only live-on-entry and therefore
adds a Use-before-overlapping-Def anti-dependence without pretending that the
new definition produces its value. Explicit `order_before` edges likewise
constrain order without creating a live value. Overlapping target definitions
remain a multiple-driver error.

The bit interval is an alias-domain choice made by the SLT adapter, not a field
of the shared analysis. `celox-analysis::interval` now supplies an exact,
unit-independent disjoint interval index, so the same representation can be
used for bits here and bytes in a machine-memory adapter. Construction is
`O(N log N)` and one overlap query is `O(log N + K)` for `K` returned
definitions; neither time nor storage depends on the numerical width of an
RTL object.

A cyclic LogicPath SCC is the point where a static definition is executed more
than once. It is therefore lowered as the existing explicit unrolled or
runtime-convergence region and acts as a synchronization fence. Observable
capture events are fences as well. The maximal acyclic runs between those
boundaries are lowering regions. Within one such region, hard dependencies
and the Def-to-Use value subset are passed to the shared bottom-up DAG list
scheduler. Scheduling a use backward makes its unscheduled definitions live;
scheduling a definition kills its value. The smallest live-value delta wins,
with critical-path and stable-index tie breakers. Reversing that order for
forward lowering keeps independent single-use chains contiguous without
attaching arbitrary HDL widths to machine VRegs.

For `N` region nodes, `E` hard edges, and `V` value edges, the list scheduler
uses `O((N + E + V) log N)` time and `O(N + E + V)` storage. It constructs no
layer, all-pairs reachability table, path cone, or interval proportional to an
address width. A 4,096-node independent-region regression exercises this
bound, and a 16 MiB interval regression confirms width-independent storage.

This ordering is not itself mem2reg. Lowering still emits the semantic Store
for a LogicPath target and Load for an input. Once their order and the SCC
boundaries are explicit, the normal SIR Store-to-Load forwarding and CFG
MemorySSA passes may replace a legal pair. Directly replacing every SLT input
with a producer register here is deliberately excluded: fanout and terminal
visibility can otherwise create the eager whole-version live ranges rejected
in Step 32. Selecting larger promoted use clusters remains an allocator-owned
decision.

Current bounded validation is: shared interval/DAG/CFG/SSA/MemorySSA analyses
39/39, parser scheduler 16/16, observer/cascade/false-loop regressions 163
passed with the known upstream ignores, optimized non-LTO library 920/920,
native testbench 60 passed with one upstream ignore, and counter 9 passed with
three Veryl ignores. Package/all-target check and strict clippy, format, diff,
and VitePress documentation gates pass.

The full trace-free non-LTO run at
`target/heliodor/results/20260720T025452Z_celox_test_soc_linux_boot.log`
completed through `reboot: Power down` and exactly one
`cy=9ae070 x3=aa pass=1`. Code generation took 163.267 s and execution took
132.472 s. The one deliberate release/LTO run at
`target/heliodor/results/20260720T030643Z_celox_test_soc_linux_boot.log` also
completed with the exact marker; code generation took 152.135 s and execution
took 133.980 s. The Heliodor checkout remained clean at pinned revision
`7ad830fc0f8506c934b61a853ce2eadfa5926b82`.

This establishes semantic acceptance, but not a throughput improvement. The
immediately preceding current-revision non-LTO sample took 164.189 s to compile
and 126.644 s to execute. A single timing pair cannot establish a regression,
but the new 132.472 s execution sample does not support a speedup claim. The
source MemorySSA order is therefore retained as the correct lowering
foundation; actual Store-to-Load use-cluster promotion and allocator-owned
range splitting remain the next performance boundary.

Status: **complete as a source-MemorySSA lowering and semantic checkpoint; no
throughput gain is claimed; use-cluster promotion remains open**.

### Step 34: Exact aggregate projections and type-safe store forwarding

The Step 33 ordering exposed a separate lowering defect. A static field
projection represented as `Slice(Input)` first lowered the entire input and
only then shifted and masked the requested field. For an 839-bit ROB entry,
copying one field therefore produced an 839-bit Load and a wide reconstruction
chain even though the memory address and exact bit interval were already
known. This was not a scheduling problem: the requested operation was one
exact range Load, but lowering discarded that information.

`lower_slice_inner` now composes the Slice range with the static Input range
before emitting SIR. A scheduler-materialized Input remains a strict boundary:
the cached snapshot is sliced first, so the optimization never replaces that
snapshot with a later Load across an intervening Store. Dynamic indexes retain
their existing override path, and static loop/input overrides are queried at
the composed range before falling back to memory.

Exact range lowering exposed adjacent same-range Store-to-Load pairs. They had
not been forwarded because SLT memory Loads conservatively have `Logic` type,
while the stored two-state value is often an unsigned `Bit`. The payload and
width are identical in a two-state simulation. Store-to-load forwarding now
accepts only the exact-width `unsigned Bit -> Logic` case when `four_state` is
false. Exact type matches remain unchanged; signed Bit values and every
four-state kind mismatch still retain the memory round trip.

The resulting ROB transfer changes from a full-entry Load followed by field
extraction and a second Store to direct use of the exact stored field value:

```text
before: Store rob_alloc_entry[133] = r557
        r = Load rob_alloc_entry[0:838]
        Store u_rob.i_alloc_entry[133] = Slice(r, 133)

after:  Store rob_alloc_entry[133] = r557
        Store u_rob.i_alloc_entry[133] = r557
```

The complete pre/post/native SIR and MIR are retained under
`target/heliodor/analysis/step34-range-lowering` and
`target/heliodor/analysis/step34-range-forwarding`. The final optimized SIR
contains the direct field transfers and no intermediate same-range Loads for
the inspected ROB copy paths.

Validation covers three range-lowering cases (direct static range, cached
snapshot, and static override), five forwarding cases including two-state,
four-state, signedness, and width boundaries, all 39 shared-analysis tests,
all 16 parser scheduler tests, all 926 library tests, native testbench 60
passed with one upstream ignore, and counter 9 passed with three Veryl
ignores. Package check, all-target strict clippy, format, and diff checks pass.

The final trace-free optimized non-LTO run completed through
`reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`. Code generation
took 156.443 s and execution took 110.254 s. The adjacent Step 33 run took
163.267 s to compile and 132.472 s to execute; the final execution sample is
16.8% shorter. The range-only intermediate measured 160.558 s compile and
112.922 s execute, so both the exact-range lowering and the subsequent direct
forwarding were measured on generated code that retained the acceptance
marker.

The one final release/LTO run also completed through `reboot: Power down` with
the exact marker. Its code generation took 149.973 s and execution took
110.762 s. LTO was used only for this final gate, not for iterative builds.

This does not eliminate every aggregate-memory cost. Inside the ROB itself,
some consumers genuinely request both a complete entry and individual fields;
those currently remain distinct memory values. Sharing that value identity is
a later aggregate-promotion/use-cluster decision and must not be approximated
by moving scheduler materialization points.

Status: **complete for exact static aggregate projection and its safe
same-range forwarding; scheduler order and Linux cycle count are preserved;
aggregate value sharing remains open**.

### Step 35: Coalesce narrow projections independently of covering loads

The aggregate projection fix in Step 34 deliberately retained consumers that
really load both a complete object and some of its fields.  The static-load
coalescer then treated the covering load as a reason to leave every overlapping
narrow load independent.  That discarded a legal local choice: narrow loads
with the same machine-word projection can share one word load even when another
consumer also requires the complete object.

`coalesce_static_loads` now groups equal narrow word projections without making
their eligibility depend on a covering wide load.  It does not replace, move,
or reorder the covering load, and it changes neither SIR scheduling nor the RTL
Store order.  A focused regression keeps the wide load and verifies that the
overlapping narrow loads still coalesce.

The complete trace is retained at
`target/heliodor/analysis/step35-word-load-coalescing`.  The non-LTO Linux run
completed through `reboot: Power down` with exactly
`cy=9ae070 x3=aa pass=1`; code generation took 155.724 s and execution took
108.799 s.  This is a small continuation of Step 34 rather than a new lowering
architecture.

Status: **complete and committed as `103c9985`; scheduler and observable RTL
semantics are unchanged**.

### Step 36: CFG-exact sparse object write state

Complete MIR inspection exposed repeated sparse-state setup before consecutive
Stores to the same RTL object.  Every Store re-tested the object's dirty state,
re-marked it active, and conditionally selected stable versus working storage,
even when a dominating Store made the object state unambiguous.  This is an
ISel state-proof problem, not a reason to reorder scheduled SIR.

`SirCfg` now implements the structure-independent `celox-analysis` SSA CFG
interface.  A pruned MemorySSA instance uses each sparse `AbsoluteAddr` as an
alias-disjoint variable and classifies a Store as:

- `First` when its use reaches entry and the complete sparse commit run is
  guaranteed to follow;
- `Active` when its use reaches a dominating Store to that object without an
  intervening commit reset; or
- `Unknown` at phis, loops, resets, and every unproved point.

The analysis reuses the existing CFG, dominators, dominance frontiers,
postdominators, and SCCs.  Its time and storage are linear in CFG edges, sparse
Store definitions, and generated phi inputs.  It creates no layer graph,
all-pairs reachability table, or representation proportional to an RTL width.
A commit is an explicit reset definition.  A Store whose following commit is
not proved, or whose block shares a cyclic SCC with that commit, keeps the old
lowering.

ISel consumes only these certificates.  A first write initializes the touched
working chunk from stable storage without reading dirty state.  A proved active
single-chunk object reads working storage directly and omits repeated active,
dirty, and summary setup.  `Unknown` executes the previous instruction sequence
unchanged.  The scheduler, CFG, SIR Store order, and commit order are not
modified.

Five CFG regressions cover straight-line definitions, mutually exclusive
diamond arms, a maybe-store join, a loop backedge, and a commit reset.  Two JIT
regressions execute the generated machine code and verify exact stable data and
cleared dirty/summary/active metadata after commit.  The current common gates
are library 934/934, native testbench 60 passed with one ignore, and counter 9
passed with three Veryl ignores.

The first-write-only intermediate passed the exact Linux marker but executed in
114.168 s and was not accepted as a speed result.  Adding the dominating active
state proof produced the complete trace at
`target/heliodor/analysis/step36-sparse-write-memoryssa`: optimized SIR is
19,602,427 bytes and complete MIR is 177,367,725 bytes.  In the inspected
`eval_comb_apply_ff[0]` body, repeated `SparseMarkActive` sequences disappear
and the emitted end address shrinks from `0x002bb110` to `0x0022f6da`, while the
spill frame remains 6,768 bytes.

The final trace-free non-LTO run at
`target/heliodor/results/20260720T055058Z_celox_test_soc_linux_boot.log`
completed through `reboot: Power down` with exactly
`cy=9ae070 x3=aa pass=1`.  Code generation took 143.852 s and execution took
108.252 s.  The execution sample is only 0.5% below Step 35, so no large speedup
is claimed.

The remaining complete-MIR defect is more specific.  Consecutive static Stores
to different chunks of one already-active object still load and test the dirty
word and load both stable and working data for every chunk.  Object MemorySSA
cannot prove that each disjoint chunk is itself a first write.  The next step is
therefore range-aware sparse MemorySSA with conservative dynamic-range kills,
not a scheduler change.

Status: **object-level first/active state is structurally complete and Linux
correct in `84fd5861`; the measured gain is small; chunk-range first-write
proof remains open**.

### Step 37: Range-aware sparse chunk MemorySSA

Step 36 still lowered every Store to a different chunk of an active object as
if that chunk might already be dirty.  It loaded the dirty word, tested its bit,
loaded both stable and working data, selected one, and only then wrote the new
value.  Complete MIR showed this sequence repeated for hundreds of consecutive
static chunks.

The first chunk implementation used one pruned-SSA variable per physical
64-bit chunk, but globally disabled that partition when the same object had any
dynamic or multi-chunk Store.  The exact target block was therefore unchanged;
its complete MIR remained 177,366,490 bytes.  Although Linux passed, execution
took 110.316 s.  That trial was rejected before commit because a definition on
an unrelated CFG path must not invalidate all static range facts for an object.

The retained design has two paths over the same object MemorySSA:

- objects containing only static single-chunk Stores use pruned chunk SSA,
  whose construction is linear in CFG edges, Stores, and phi inputs;
- mixed objects use an access-chain range solver.  Exact Store ranges either
  clobber or forward the preceding memory version, a dynamic range yields
  `Unknown` only when it actually reaches the query, reset yields `Clean`, and
  a MemoryPhi unions the states of its incoming edges.

The range solver processes all query points for one `(object, chunk)` together.
For an object with `D` definitions, `F` phi inputs, and `Q` queried static
chunks, its worst-case query time is `O(Q(D + F))`; reusable state is
`O(D + F)`.  The exact-only fast path avoids this product for the common case.
Neither path expands a dynamic Store over every possible chunk or allocates
storage proportional to an RTL numerical width.  Phi propagation is a
two-state monotone worklist, so each clean/dirty bit reaches an edge at most
once.

ISel consumes three chunk states.  `Clean` initializes working data directly
from stable storage, `Dirty` uses existing working data without preparation,
and `Unknown` retains the previous dirty test and stable/working select.  The
object active-list proof remains separate.  No SIR instruction, CFG edge,
scheduler decision, Store order, or commit order is changed.

Eight focused CFG/MemorySSA regressions cover straight-line clean and dirty
chunks, all-dirty and mixed diamond joins, a loop backedge, commit reset,
reaching dynamic alias, a dynamic Store on a mutually exclusive sibling path,
and exact multi-chunk overlap.  An executable JIT regression performs both
disjoint first writes and a repeated write, then verifies stable data and the
cleared dirty, summary, and active metadata.  Common gates pass with library
938/938, native testbench 60 passed with one ignore, counter 9 passed with
three Veryl ignores, all-target check, and strict clippy.

The complete trace is retained at
`target/heliodor/analysis/step37-range-memoryssa`.  Pre/post/native SIR is
unchanged from Step 36.  Full MIR falls from 177,367,725 to 152,861,272 bytes.
In the inspected region 228 Store sequence, the per-chunk dirty test, working
load, and stable/working select disappear.  The main emitted body ends at
`0x001d1224` instead of `0x0022f6da`.  Trace code generation took 118.702 s.

The final trace-free non-LTO run at
`target/heliodor/results/20260720T064100Z_celox_test_soc_linux_boot.log`
completed through `reboot: Power down` with exactly
`cy=9ae070 x3=aa pass=1`.  Code generation took 117.173 s and execution took
107.836 s.  Relative to Step 36, compile time is 18.5% shorter while the single
execution sample is only 0.4% shorter; no large runtime gain is claimed.

The retained MIR now exposes the next direct cost: each clean chunk still
loads, ORs, and stores the same dirty word and the same summary word.  Range
proof removed the data-selection work, but metadata updates remain
uncoalesced.  Dirty-word/summary-word state and safe update batching are the
next lowering boundary; changing the scheduler is neither required nor
allowed by this result.

Status: **range-aware chunk state is Linux-correct and materially reduces MIR
and compile time; runtime improvement remains small; repeated metadata-word
updates remain open**.

### Step 38: Hierarchical sparse metadata state

Step 37 removed the stable/working data selection but retained two metadata
read-modify-write sequences per clean chunk: one for the chunk's dirty word and
one for the dirty word's bit in the summary word.  Once any chunk in a dirty
word has been written, that summary bit is already set.  Repeating the summary
load, OR, and store for every later chunk cannot affect commit behavior.

The range MemorySSA solver now also queries the 64-chunk interval represented
by each dirty word.  This is the same CFG and alias proof used for data chunks,
not a block-local counter.  Its state immediately before a Store has three
uses:

- `Clean`: the dirty word is zero, so its first bit can be stored without
  loading the old dirty word;
- `Dirty`: at least one bit is already set, so the dirty word is preserved but
  the summary update is omitted completely; and
- `Unknown`: both metadata words retain their previous read-modify-write path.

An object-level first write still initializes both words directly.  A repeated
write to an already-dirty chunk still performs no metadata preparation.  Thus
the hierarchy is object active state, dirty-word state, and exact chunk state;
none is inferred from another at a weaker granularity.  The change emits fewer
internal metadata instructions at the same SIR Store point.  It does not move a
Store, defer metadata across an instruction, or alter CFG/scheduling.

The existing eight CFG/range regressions now assert dirty-word states at
straight-line, diamond, loop, reset, sibling-dynamic, and exact multi-chunk
points.  The executable JIT regression additionally requires exactly one
summary update for two disjoint chunks in the same dirty word, executes a third
repeated write, and verifies final stable and cleared metadata state.  Library
938/938, native testbench 60 passed with one ignore, counter 9 passed with three
Veryl ignores, all-target check, and strict clippy pass.

The complete trace at
`target/heliodor/analysis/step38-sparse-metadata-state` took 113.106 s.  Full
MIR falls from 152,861,272 to 143,093,464 bytes.  In the inspected region 228
sequence, summary offset `68373480` is stored once after the first chunk rather
than loaded and stored for every chunk.  The main emitted body ends at
`0x001abb58` instead of `0x001d1224`.

Two trace-free non-LTO runs completed through `reboot: Power down` with exactly
`cy=9ae070 x3=aa pass=1`:

- `target/heliodor/results/20260720T065710Z_celox_test_soc_linux_boot.log`:
  compile 111.882 s, execute 109.449 s;
- `target/heliodor/results/20260720T070120Z_celox_test_soc_linux_boot.log`:
  compile 111.514 s, execute 105.793 s.

The execution samples straddle the Step 37 result of 107.836 s, so the runtime
effect is not claimed as a stable large gain.  The generated-code and compile
improvements are direct.  Complete MIR now leaves one dirty-word load/OR/store
per clean chunk; coalescing those exact static masks without extending value
live ranges or changing scheduler order is the next boundary.

Status: **hierarchical summary updates are eliminated where proved redundant;
Linux semantics and tick count are preserved; dirty-word update coalescing
remains open**.

### Step 39: Explicit indexed metadata RMW

Step 38 still represented every preserved dirty bitmap update as a separate
indexed Load, register OR, and indexed Store.  The loaded metadata value then
participated in the surrounding RTL live-range problem even though the value
is simulator-private and has no SIR consumer.

Native MIR now has an explicit non-atomic `OrStoreIndexed` operation for
`[base + offset + index] |= value`.  Its shared memory-effect description is
both a read and a write over the exact alias envelope.  The MIR scheduler
treats it as a scheduling-region barrier, so an earlier read, the RMW, and a
later read cannot be reordered.  Emission uses one x86 memory-destination OR;
no RTL Store, CFG edge, scheduler decision, or commit point is changed.

The complete trace at
`target/heliodor/analysis/step39-indexed-or-rmw` has 141,698,513 bytes of MIR,
down from 143,093,464 bytes in Step 38.  The trace-free non-LTO Linux run at
`target/heliodor/results/20260720T072104Z_celox_test_soc_linux_boot.log`
completed through `reboot: Power down` with exactly
`cy=9ae070 x3=aa pass=1`; compilation took 110.487 s and execution took
110.961 s.  This establishes correctness and shorter metadata live ranges,
not a stable execution-speed gain.

Status: **explicit indexed bitmap RMW is complete; scheduler memory order and
Linux semantics are preserved**.

### Step 40: Straight-line dirty-word batching

Complete MIR after Step 39 still contained one dirty-word RMW for every clean
chunk.  Range MemorySSA already proves the exact clean state of each chunk, so
several straight-line Stores to chunks in one dirty word can share one bitmap
mask without keeping a loaded metadata value live.

The sparse write analysis now records a metadata placement action at each SIR
Store.  A batch is formed only inside one basic block for one
`(object, dirty-word)` run whose chunks are independently proved clean.  Data
Stores remain at their original SIR positions.  Only inaccessible simulator
metadata is deferred to the final Store in the run, where one constant mask is
stored or ORed.  A different Store, commit, runtime/capture event, block end,
unknown range, or changed object/word closes the batch.  The scan is linear in
the block's instructions and creates no pairwise dependence relation.

The complete trace at
`target/heliodor/analysis/step40-dirty-word-batches` has 133,989,572 bytes of
MIR.  The non-LTO run at
`target/heliodor/results/20260720T074044Z_celox_test_soc_linux_boot.log`
completed through kernel power-down with exactly `cy=9ae070 x3=aa pass=1`;
compilation took 108.349 s and execution took 112.165 s.  MIR and compile time
shrank, but this execution sample is slower than Step 38's range, so no runtime
gain is claimed.

Status: **proved straight-line metadata batches are complete; RTL Store order,
the MIR scheduler, and the Linux tick count are unchanged**.

### Step 41: Preserve profitable native array element layout

The post-optimization SIR exposed a circular layout decision.  The native
backend requests element-strided storage for unpacked arrays, but the SIR load
and store coalescers first combined adjacent logical elements into packed
64-bit accesses.  Layout discovery then saw those manufactured cross-element
accesses and rejected element-strided storage.  For Heliodor's 32-entry
12-bit `sh_csr_addr`, this produced six packed word loads followed by shift and
mask extraction instead of independent scalar element loads.

Layout intent is now an explicit input to SIR optimization.  Public
`compile_to_sir`, Cranelift, NAPI, and Wasm retain the ordinary packed path.
Only a native `ElementStrided` build asks coalescing to preserve eligible
element boundaries.  Loads and Stores may still coalesce within one element;
they may not manufacture an access across two elements whose scalar layout is
being retained.  The native post-merge cleanup receives the already selected
layout and applies the same rule.  No scheduler order or CFG is changed.

Unconditionally padding every memory-like array caused unacceptable IR and
memory growth on 4,096-entry tag arrays.  That trial was rejected.  Until a
compact bulk-store MIR operation exists, the retained profitability boundary
is a padded 9--64-bit element in a register-like array whose plane is at most
256 bytes.  This bounds the extra scalar SIR by the existing small array size;
large memories keep packed bulk lowering.

The complete trace at
`target/heliodor/analysis/step41-preserve-small-element-layout` has
19,628,017 bytes of post-optimized SIR, 19,655,322 bytes of native SIR, and
133,726,550 bytes of MIR.  The inspected `sh_csr_addr` sequence contains 32
direct element Loads instead of six covering Loads plus roughly 52 extraction
operations.  The non-LTO run at
`target/heliodor/results/20260720T090921Z_celox_test_soc_linux_boot.log`
completed through kernel power-down with exactly `cy=9ae070 x3=aa pass=1`;
compilation took 105.731 s and execution took 108.420 s.

Status: **the native layout/optimization contract is explicit and bounded;
packed backends and scheduler ordering are unchanged**.

### Step 42: Direct whole-element indexed accesses

After Step 41, a dynamic 12-bit element Load used the correct 16-bit physical
slot but still emitted an AND, while a dynamic Store retained a load/bitfield
insert/store sequence.  A zero-displacement `SIROffset::Element` whose width is
the complete logical element can use the slot directly.  ISel now emits one
naturally sized indexed Load or Store for that case in both value and mask
planes.  Stores mask the logical source and may canonicalize backend-owned
padding; padding preservation is not part of RTL semantics.  Partial-element
accesses retain the existing RMW path.

An executable JIT regression uses a two-element 12-bit array, poisons the
padding, performs a dynamic Store and Load, and verifies the two logical
values and canonicalized physical slot.  The older sparse one-bit regression
was corrected to assert only RTL bits and adjacent logical elements rather
than imposing a non-semantic padding-preservation rule.

The final complete trace at
`target/heliodor/analysis/step42-final-target-scoped-elements` is byte-identical
to the original Step 42 trace for all four outputs.  Its sizes and SHA-256
hashes are:

- pre-optimized SIR: 58,711,247 bytes,
  `867e5df4cb8fda6c1cbc564bd5ff7d9ef34dc7b7a126960301d89e536f5bc52e`;
- post-optimized SIR: 19,628,017 bytes,
  `079510d983473a2ba1b878d6d04e17b14f92959e2bdc4c7ca99847eb3b634dab`;
- native optimized SIR: 19,655,322 bytes,
  `f1cc8a47869bc68027717811fd15ea6ddd0b0f2111ae3d76725202eaef1f24c4`;
- MIR: 133,426,760 bytes,
  `f9fb5b9f44b1f1ff285325fbbf259a642eff51a5cc0ceef446a7e369aeb3a3cd`.

Final combined validation is native backend 442/442, optimized non-LTO library
947/947, native testbench 60 passed with one upstream ignore, counter 9 passed
with three Veryl ignores, all-target check, strict clippy, format, and diff
checks.  The exact final-source non-LTO Linux run at
`target/heliodor/results/20260720T093721Z_celox_test_soc_linux_boot.log`
completed through `reboot: Power down` and `cy=9ae070 x3=aa pass=1`.
Compilation took 107.693 s and execution took 105.529 s.  Two post-layout runs
are close to 108.4 and 105.5 s, but the Step 38 range was 105.8--109.4 s, so a
large stable runtime gain is not claimed.

Status: **whole-element native accesses are direct and Linux-correct; the
scheduler is unchanged; larger structural execution-time gaps remain open**.

### Step 43: Commit-independent sparse whole-zero fills

Step 42 deliberately kept the 256-byte plane limit on SIR element-boundary
preservation.  Removing that limit before adding a compact bulk operation made
the 4,096-entry BTB arrays retain thousands of scalar Stores through every SIR
pass and did not finish in a useful time.  With the limit retained, optimized
SIR instead exposes the reset of the 51-bit `tag` array as one 208,896-bit
`Concat` of an exact zero followed by one whole-object sparse Store.  Generic
strided Store lowering expanded that value into hundreds of thousands of MIR
instructions.

The shared SIR representation now has demand-driven exact-zero proof.  It
builds one definition index, starts only from candidate Store sources, and
walks the reachable zero-preserving operand graph with an explicit stack.  It
does not materialize the represented bit vector or build a reverse-use graph
for unrelated SIR.  Work and memory are linear in the indexed definitions and
visited operand edges; the 4,096 repeated `Concat` operands are traversed
without recursion.

Element-strided layout may now retain a padded array when an otherwise
unsupported static Store is a complete, exact-zero overwrite of sparse
working state with no trigger or capture side effect.  Native ISel replaces a
complete covered zero run with one physical `MemFill`, marks the object active,
and writes the complete dirty and summary bitsets.  Value and mask planes are
both cleared in four-state mode.  Backend-owned padding is canonicalized to
zero; no RTL bit is added to the observable state.

This transformation does not preserve an artificial source order.  Stores to
other objects and runtime events may lie between members of the zero run.  A
read, non-zero or dynamic Store, or Commit of the same object closes the run,
because those operations can observe or replace that object's working state.
The new MIR operation publishes its exact write range to the shared dependence
analysis, so the pressure scheduler may move it across disjoint work while
retaining every overlapping RAW, WAR, and WAW edge.  Its x86 emitter preserves
all scratch registers around `rep stosq` and handles the 4/2/1-byte tail.

The first workload integration accidentally made this optimization conditional
on finding one local sparse worklist Commit run.  `eval_only_ff` intentionally
publishes from a separate apply function, so its wide reset missed the
optimization: ISel produced 758,042 MIR instructions and 744,821 VRegs;
`mir_opt` alone took 149.545 s and late state forwarding took 39.015 s.  The
trace-free compile took 236.522 s.  Whole-zero planning is now independent of
the commit strategy, which is the semantic boundary it actually requires.

After the commit-strategy correction, the trace-free non-LTO compile-only run at
`target/heliodor/results/20260720T105332Z_celox_test_soc_linux_boot.log` took
64.899 s, versus 107.693 s in Step 42.  The final release/LTO compile-only run
at `target/heliodor/results/20260720T105852Z_celox_test_soc_linux_boot.log`
took 62.750 s.  Those two compile-only samples precede the final change which
lets the pressure scheduler move `MemFill` by its exact memory dependencies.
The final-source release/LTO full run at
`target/heliodor/results/20260720T111304Z_celox_test_soc_linux_boot.log`
reached `reboot: Power down` and exactly `cy=9ae070 x3=aa pass=1`; compilation
took 62.047 s and execution took 111.351 s.  The compile-time reduction is
direct, but this execution sample does not establish a runtime gain.

Final library tests are 957/957 and the native-backend subset is 450/450,
including the scheduler and executable lowering regressions.  All-target check
and strict clippy also pass.

Status: **the giant exact-zero value and Store no longer enter MIR, commit
placement is not treated as an RTL rule, overlapping memory order is proved by
exact effects, and Linux semantics and tick count are preserved; the larger
execution-time gap remains open**.

### Step 44: Machine-width known-bits mask elimination

The complete generated MIR exposed a systematic ordinary-compiler defect in
the hot FPU and vector bodies.  `lower_to_imm_forms` recognized constant
operands of 64-bit `And`, but not `And32`; the following redundant-mask pass
understood only 64-bit `AndImm` and represented facts as one contiguous source
width.  Explicit 32-bit ALU zero-extension and register-form masks therefore
escaped the pass.  One repeated hot sequence in Step 42 was effectively:

```text
v77599 = sub.w32 v16510, v44036
v16515 = and.w32 v77599, 0xffffffff
v16516 = and.w32 v16515, v44135  # v44135 is 0xffffffff
v16517 = select v16512, v16516, v16510
v16518 = and.w32 v16517, 0x3fffffff
v16519 = and.w32 v16518, v2251   # v2251 is 0x3fffffff
```

The retained MIR pass lowers a constant `And32` operand to `AndImm32` and uses
a conservative possible-one-bits lattice over the two actual MIR machine
widths.  It handles immediate and register forms, folds mask chains, and
recognizes idempotent repeated operands.  If a low-word mask is redundant but
the source may have upper bits, it emits `Mov32`, not `Mov`, preserving the
observable 32-bit zero-extension.  An unchecked `Bsr` remains completely
unknown because its zero-input result is unspecified.  Facts and
definition-chain rewrites are block-local; the definition table stores only
compact mask facts, and only immutable constant values are looked up
function-wide.  No instruction, memory operation, or scheduler edge is
reordered.

The corresponding final hot sequence is:

```text
v77599 = sub.w32 v16510, v44036
v16517 = select v16512, v77599, v16510
v16518 = and.w32 v16517, 0x3fffffff
```

This relies only on MIR value semantics.  It does not preserve source order,
padding contents, or any other non-semantic rule; disjoint RTL work remains
free for later scheduling and allocation.

Before this retained change, a source-MemorySSA direct Store-to-Load forwarding
trial was measured and fully reverted.  It kept the packed Store home while
also carrying the forwarded SSA value across a long use range.  The exact
Linux workload still passed but execution regressed from the immediately
preceding 111.069 s baseline to 116.634 s.  That double-residency design is not
part of this step.

The exact final-source non-LTO run completed through `reboot: Power down` with
`cy=9ae070 x3=aa pass=1`; compilation took 68.337 s and execution took 107.693
s, 3.0% below the preceding non-LTO execution sample.  Two final-source
release/LTO runs produced the same marker.  They compiled in 61.215 and 62.130
s and executed in 109.005 and 107.524 s.  Against Step 43's release/LTO
111.351 s execution sample, those are 2.1% and 3.4% reductions.  Earlier
byte-identical-MIR candidates executed in 100--102 s, but that host-time
variation is not used as the final speed claim.

The complete final trace is at
`target/heliodor/analysis/step44-known-bits-mask-final-compact`:
pre-optimized SIR, post-optimized SIR, native-optimized SIR, and full native
MIR are all present.  Focused MIR optimization tests pass 61/61, the
optimized library passes 962/962, native testbench passes 60 with one upstream
ignore, and counter passes 9 with three Veryl ignores.  Package check,
all-target strict clippy, format, and diff checks pass.

Status: **redundant machine-width normalization is removed without constraining
RTL scheduling freedom; Linux semantics and tick count are preserved and the
two final-source release execution samples improve by 2.1--3.4%**.

### Step 45: Allocator-visible same-block value sharing

The complete Step 44 MIR and disassembly exposed a second ordinary-compiler
defect in a hot indexed-access loop. Four loads using the same dynamic bit
index independently computed both parts of that index:

```text
v115577 = shr v25816, 3
v115578 = and.w32 v25816, 0x7
load.i8 [sim + 33900268 + v115577]
v115581 = shr v25816, 3
v115582 = and.w32 v25816, 0x7
load.i8 [sim + 33900444 + v115581]
...
```

GVN already assigned equal value numbers to these expressions, but its old
Step 15 pressure guard deliberately replaced a dead same-block leader with
each later recomputation. Constant operands also remained in register-register
form until after the final GVN invocation, so a shift or mask looked like an
arbitrary two-input expression instead of a target operation for which the
allocator has an exact rematerialization recipe.

The final high-pressure optimization iteration now lowers constant operands
before GVN. GVN may reuse a dead same-block leader for exact one-source
rematerializable operations and for an exact-version `SimState` load; arbitrary
binary operations and cross-block live-range extension retain the previous
policy. `AndImm32` has its own GVN opcode so its zero-extending 32-bit semantics
cannot be confused with 64-bit `AndImm`. A state load is still keyed by its
structural MemorySSA version, and allocator reconstruction independently
checks the version at each selected use.

The corresponding hot block now contains one byte index and one bit index:

```text
v115577 = shr v25816, 3
v115578 = and.w32 v25816, 0x7
load.i8 [sim + 33900268 + v115577]
load.i8 [sim + 33900444 + v115577]
load.i8 [sim + 33900272 + v115577]
load.i8 [sim + 33900448 + v115577]
```

Its x86-64 body likewise computes `shr index, 3` and `and index, 7` once and
feeds all four indexed loads. No scheduler rule, source order, RTL padding, or
effect order was added. The only semantic conditions are value equality and,
for loads, the existing MemorySSA version. Pressure scheduling and allocation
remain free to carry, split, rematerialize, or home the shared value.

All three SIR dumps are byte-identical to Step 44. Full MIR falls from
62,953,833 to 61,343,441 bytes, and the final fused function's emitted code
falls from 1,003,128 to 980,233 bytes. This is not uniformly free: two smaller
spill frames rise from 88 to 240 bytes and from 40 to 232 bytes, while the
final frame rises from 5,368 to 5,376 bytes. The retained result is therefore
a value-representation improvement, not evidence that register pressure is
solved.

Every full run reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`. Unpinned candidate execution samples ranged from
99.986 to 107.090 s. A fixed-CPU non-LTO A/B/A/B comparison separated compile
and execute time: Step 44 executed in 101.001 and 101.731 s, while this step
executed in 98.888 and 104.028 s. Their means, 101.366 and 101.458 s, establish
no throughput improvement. No runtime-speed claim is made from this step.

The complete final trace is at
`target/heliodor/analysis/step45-rematerializable-gvn-final`. Focused MIR
optimization tests pass 64/64, the optimized library passes 965/965, native
testbench passes 60 with one upstream ignore, and counter passes 9 with three
Veryl ignores. Package all-target check, strict clippy, format, and diff checks
pass.

Status: **same-block target-rematerializable values are represented once,
without adding a non-semantic RTL order; Linux semantics and tick count are
preserved, but measured execution throughput is unchanged and the allocator
pressure problem remains open**.

### Step 46: Control-dependent masked array search

The complete optimized SIR exposed an eager predicate in `pending_xlate` of
the following form:

```text
any(outer & (flags | (gate &
    (concat(array[lane] == 0x100, ...)
     | concat(array[lane] == 0x300, ...)
     | concat(array[lane] == 0x180, ...)))))
```

The packed form unconditionally emitted 32 static `sh_csr_addr` element loads
and 96 equality operations. In two-state logic it is instead a search over the
set bits of `outer & gate`, preceded by the independent `outer & flags` test.
The new `masked_array_any` SIR pass recovers that control dependence: it exits
on an active flag, computes the candidate mask, selects one set lane with CTZ,
performs one dynamic element load, short-circuits the key comparisons, and
clears the lane with `remaining &= remaining - 1` only after all keys miss.

This does not add a source-, layer-, block-, or arm-order rule. Those orders
are optimization freedom. The legality proof uses only the actual RTL
semantics: all recognized definitions dominate the reduction in one block;
the unpacked-array shape and exact lane-to-offset mapping agree with program
metadata; the eager load/compare DAG has no outside users; and no overlapping
Store or Commit changes the searched bit range between the original loads and
the reduction. A dynamic write is conservatively treated as overlapping. The
four-state case is left unchanged. The scheduler and its dependency graph are
not modified.

The complete final trace is at
`target/heliodor/analysis/step46-masked-array-any-final`. Its
post-optimized SIR and full MIR contain the recovered branch/loop, one dynamic
element-load site, and one comparison site per key; the old 32-load/96-compare
body is dead. It is byte-identical to the dump used for the fixed-CPU A/B
measurement. Six focused tests cover semantic equivalence, actual load counts,
four-state rejection, declared array shape, lane order, and overlapping versus
subsequent writes; a seventh test fixes the CLI name and optimization-level
defaults. The optimized library passes 972/972, native testbench passes 60
with one upstream ignore, and counter passes 9 with three Veryl ignores.

Every full run reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`. A fixed-CPU non-LTO parent/candidate/parent/candidate
comparison separated code generation from execution. Step 45 executed in
106.589 and 102.699 s; this step executed in 100.532 and 98.778 s. Their means
are 104.644 and 99.655 s, a 4.989 s or 4.8% reduction. The paired reductions
are 6.057 and 3.921 s. Compile times were recorded separately: 94.476 and
91.564 s for Step 45, and 91.762 and 90.653 s for this step; no compile-speed
claim is made. After the final source checks, a further non-LTO run compiled in
90.686 s and executed in 99.781 s with the same completion marker.

Status: **eager masked array comparisons are represented as a
control-dependent search without constraining RTL scheduling freedom;
Linux semantics and tick count are preserved and fixed-CPU execution improves
by 4.8%**.

### Step 47: CFG circular-priority recovery

The complete optimized SIR exposed another unconditional hot loop at the
start of every combinational evaluation. It scanned all 32 ROB lanes, loaded
five one-bit arrays per iteration, formed
`valid & ((is_store & is_amo) | is_fence | is_cbo_zero)`, and retained the
smallest `(lane - head_idx) & 31` age. In two-state logic this is exactly a
packed candidate mask rotated by `head_idx`, followed by a nonzero test and
CTZ.

The new `circular_priority` SIR pass discovers this from the natural CFG loop
and its SSA recurrences. It does not preserve source, layer, block, or branch
arm order: those remain optimization freedom. Legality depends only on the
observable RTL semantics. The loop must be a pure counted power-of-two scan;
the index, found flag, best age, update predicate, and circular age expression
must match; the counter and index widths must enumerate the full domain; the
packed predicate may contain declared one-bit unpacked-array loads and pure
Boolean operations; and four-state execution is left unchanged. Definitions
used after a removed loop are found by one whole-EU linear use walk. Pure
loop-invariant dependency closures are moved out of the loop, while escaping
induction values, memory reads, or effects reject the rewrite. This avoids both
the former blanket escape rejection and a loop-count-times-EU-size analysis.
The scheduler and its dependency graph are unchanged.

The complete final trace is at
`target/heliodor/analysis/step47-circular-priority-final`. In both standalone
`eval_comb` and scheduler-used `eval_comb_apply_ff`, the old 32-iteration
backedge and dynamic bit loads are gone. The optimized SIR and MIR contain five
32-bit static loads, packed mask operations, a rotate by `head_idx`, a nonempty
branch, and CTZ. Ten focused tests exhaustively compare the original and
rewritten four-lane CFG, cover redundant index normalization and
escaped-invariant hoisting, and reject four-state, narrow or non-unit
induction, undersized arrays, side effects, and escaping loop-variant values;
the CLI/default test is included. The optimized library passes 982/982, native
testbench passes 60 with one upstream ignore, and counter passes 9 with three
Veryl ignores. All-target check, strict clippy, format, and diff checks pass.

All four fixed-CPU non-LTO parent/candidate/parent/candidate runs reached
`reboot: Power down` and exactly `cy=9ae070 x3=aa pass=1`. Parent execution was
98.925 and 106.973 s; candidate execution was 102.461 and 101.181 s. Their
means are 102.949 and 101.821 s, a 1.127 s or 1.1% reduction, but the paired
changes have opposite signs: candidate is 3.537 s slower in the first pair and
5.791 s faster in the second. Compile times were recorded separately: parent
90.882 and 92.263 s, candidate 93.047 and 91.607 s. No compile-speed or
execution-speed claim is made from these samples. The one final release/LTO
qualification compiled Heliodor in 85.770 s and executed it in 101.637 s with
the same tick and power-down markers.

Status: **the unconditional 32-lane circular-priority scan is represented by
its packed dataflow meaning without adding a non-semantic order; Linux meaning
and tick count are preserved, while the measured runtime effect remains
unconfirmed because paired samples disagree**.

### Step 48: Schedule indexed writes and sparse marks by their effects

The production pressure scheduler still split a basic block at every
`StoreIndexed`, `OrStoreIndexed`, and `SparseMarkActive`, even though all three
already publish their memory effects to the shared byte-range dependence
analysis. In the generated `eval_comb_apply_ff` MIR these barriers occurred
inside nearly every dynamic next-state update. They prevented independent
producer chains from being placed next to their indexed stores and imposed an
order which is not part of RTL semantics.

These operations now remain in the block-local scheduling DAG. Bounded indexed
accesses use their conservative alias envelope; unbounded accesses cover their
complete direct-memory object. `SparseMarkActive` uses its exact active-count,
flag, and list ranges. The existing dependence tracker therefore preserves
overlapping RAW, WAR, and WAW order, including the read-modify-write nature of
an indexed OR and the order of sparse worklist insertions, while disjoint
operations and read/read pairs remain free to move. Def-use edges keep the
allocated sparse scratch value attached to its mark.

Moving a sparse mark to the end of a machine fallthrough block exposed an
emitter defect: the pseudo's internal done label and the elided jump's target
label denoted the same machine-code position, but the assembler permits only
one label per instruction. The mark now reuses the fallthrough continuation
label. An executable two-block regression covers this exact placement and
checks the active count, flag, and list contents.

The scheduler regressions pass 19/19, optimized library tests pass 987/987,
native execution tests pass 16/16, native testbench passes 60 with one upstream
ignore, and counter passes 9 with three Veryl ignores. The complete final trace
is at `target/heliodor/analysis/step68-effect-dag`. Its SIR is unchanged. The
full MIR trace falls from 60,336,354 to 60,304,632 bytes, and the standalone
`apply_ff` spill frame falls from 232 to 216 bytes; the main fused frame remains
5,368 bytes. Inspection of the scheduled MIR shows dynamic producer/load/store
chains closing before the following sparse mark instead of being pinned to
their source order.

The non-LTO Linux gate compiled in 66.524 s and executed in 103.476 s. It
reached `reboot: Power down` and exactly `cy=9ae070 x3=aa pass=1`. This matches
the 103.471 s best adjacent baseline sample, so no execution-speed claim is
made. The retained result removes non-semantic scheduling barriers, but the
unchanged fused spill frame and runtime show that working-state round trips,
not these barriers alone, are the next larger target.

Status: **indexed writes and sparse metadata marks use exact dependence edges
instead of artificial region boundaries; Linux semantics are preserved and
generated MIR is smaller, but measured execution is unchanged**.

### Step 49: Publish hazard-free sparse next state directly

Dynamic FF array writes enter SIR in `SPARSE_WORKING_REGION`. The native
backend previously copied the current stable chunk on the first write, updated
the active-region/dirty/summary metadata, wrote the sparse chunk, and drained
the active worklist at the event tail even when no operation could observe the
old stable value between the Store and Commit. The existing working-round-trip
pass did not consider this form at all; it only recognized seeded
`WORKING_REGION` state and rejected dynamic offsets.

The complete-event hazard analysis now models the interval from each normal or
sparse next-state Store to its matching publication. A STABLE read during that
interval is an old-state hazard. An overlapping STABLE write is a publication-
order hazard. The matching Commit closes the interval, so reads after it are
correctly allowed, and an exit with an unpublished write is rejected. Dynamic
and element offsets conservatively alias the complete object. This is a
forward may-dataflow problem over the actual CFG; it does not preserve source,
EU, block, or layer order beyond real dependencies.

For a sparse object with a valid full-range tail Commit and no such hazard, all
sparse Stores are redirected to STABLE and the Commit is removed. Sparse state
has no per-EU seed, so multiple producer EUs are not an additional legality
condition: their Stores retain merged event order, and the same CFG analysis
rejects any intervening observation or competing write. In the generated
Heliodor SIR, for example, the indexed writes to `inst38.var26` become ordinary
region-0 Stores in both `eval_apply_ff` and `eval_comb_apply_ff`; their tail
Commits and associated sparse metadata paths disappear.

Redirecting a sparse whole-object zero overwrite initially bypassed the
existing sparse zero-fill recognition and expanded reset code into thousands
of scalar STABLE Stores. Zero-fill recognition now also accepts a direct
STABLE Store when the object retains sparse-origin layout metadata, and emits
one physical `MemFill` without sparse metadata. A focused plan test and an
executable ISel/pass regression cover this path.

Focused round-trip and publication tests pass 4/4 and hazard tests pass 7/7.
The optimized library passes 996/996. Dynamic-NBA tests pass 33 with one
upstream ignore, cross-block NBA tests pass 11 with one ignore, and flip-flop
tests pass 200 with 42 upstream ignores. Native execution passes 16/16, native
testbench passes 60 with one upstream ignore, and counter passes 9 with three
Veryl ignores.

The final complete trace is at
`target/heliodor/analysis/step69-sparse-direct-final`. Native optimized SIR
falls from 19,619,580 to 19,548,386 bytes. The full MIR trace falls from
60,304,632 to 53,009,418 bytes after retaining bulk-zero lowering, a 12.1%
reduction.

Both non-LTO no-dump runs reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`. They compiled in 73.027 and 72.968 s and executed in
93.902 and 92.780 s. Against Step 48's adjacent 103.476 s execution, the mean
is 93.341 s: 10.135 s or 9.8% faster. Compile and execute times are kept
separate; compile remains slower than Step 48 and no compile-speed claim is
made.

Status: **hazard-free dynamic FF state bypasses sparse copy/metadata/commit
machinery without adding a non-semantic ordering rule; Linux meaning and tick
count are preserved and non-LTO execution improves by 9.8%; remaining compile
time is a separate target**.

### Step 50: Register-free sparse active bitmap

Sparse regions which cannot use Step 49's direct-publication proof still need
event-local registration before their dirty chunks are committed.  The old
registration expanded every `SparseMarkActive` into a flag load and branch,
flag store, active-count load and capacity branch, descriptor-index store, and
count increment/store.  It also defined a scratch VReg, extending allocation
pressure around every remaining sparse write.  This machinery implemented an
idempotent set with a byte flag plus a counted index list.

The registration state is now one fixed bitmap.  A mark has no VReg operands
and emits one `bts qword [sim + active_word], immediate_bit`.  The event-tail
worklist walks the five bitmap words needed by Heliodor's 280 descriptors,
clears each word before visiting its set bits, and ignores malformed padding
bits in the final word.  Memory effects name the exact eight-byte bitmap word,
so the scheduler retains same-word read-modify-write dependencies without
adding a global sparse-order barrier.  This changes only private native
runtime metadata; optimized SIR is byte-identical to Step 49.

Focused tests cover register-free marking across a live value, fallthrough
placement, repeated marks, later bitmap words, final-word padding, exact memory
effects, GVN invalidation, reload recipes, and scheduler dependencies.  The
optimized library passes 997/997.  Dynamic-NBA tests pass 33 with one upstream
ignore, cross-block NBA tests pass 11 with one ignore, flip-flop tests pass 200
with 42 ignores, native execution passes 16/16, native testbench passes 60 with
one ignore, and counter passes 9 with three Veryl ignores.

The complete trace is at
`target/heliodor/analysis/step70-active-bitmap`.  Native optimized SIR remains
19,548,386 bytes.  Full MIR falls from 53,009,418 to 52,743,850 bytes, and the
emitted `eval_comb_apply_ff` body falls by 15,031 bytes.  Its spill frame remains
`0x1500`, confirming that this does not repair the larger live-range and edge-
copy problem.

Both trace-free non-LTO runs reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`.  They compiled in 80.238 and 79.904 s and executed in
91.148 and 95.338 s.  The 93.243 s execution mean is effectively unchanged
from Step 49's 93.341 s mean, so no runtime-speed or compile-speed claim is
made.

Status: **remaining sparse registration is reduced to a register-free bitmap
set with exact dependencies; Linux meaning and tick count are preserved and
generated code is smaller, but measured execution is unchanged**.

### Step 51: Fold machine-width algebraic identities

Inspection of the complete optimized MIR found full-word FF copies still
lowered as `load dst; and.w32 dst, 0; or.w64 src; store dst`.  The emitted x86
therefore loaded the overwritten destination, zeroed it, ORed in the source,
and stored it.  This was not a register-allocation artifact: the sequence was
already present after MIR optimization.

The MIR constant folder and algebraic simplifier handled only the 64-bit ALU
variants.  They now also model `Add32`, `Sub32`, `Mul32`, `And32`, `Or32`,
`Xor32`, and `AndImm32`.  Constant results explicitly truncate both operands
to 32 bits and zero-extend the result.  Identity rewrites produce `Mov32`, not
`Mov`, so an arbitrary 64-bit source cannot silently retain its upper half;
the existing known-bits proof may remove that `Mov32` only when the source is
already known to fit.

The generated full-word sequence is now exactly `load src; store dst`, and the
overwritten destination load is removed before register allocation.  Focused
tests cover the concrete masked-merge chain, every 32-bit ALU constant fold,
and every identity while retaining zero-extension.  The library passes
1000/1000.  Dynamic-NBA tests pass 33 with one upstream ignore, cross-block NBA
tests pass 11 with one ignore, flip-flop tests pass 200 with 42 ignores, native
execution passes 16/16, native testbench passes 60 with one ignore, and counter
passes 9 with three Veryl ignores.

The complete trace is at
`target/heliodor/analysis/step71-word32-algebraic`.  Optimized SIR remains
byte-identical at 19,548,386 bytes.  Full MIR falls from 52,743,850 to
52,213,544 bytes, and the emitted `eval_comb_apply_ff` body falls by 3,435
bytes.  Its spill frame remains `0x1500`.

Both trace-free non-LTO runs reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`.  They compiled in 80.586 and 80.420 s and executed in
93.621 and 93.701 s.  The 93.661 s mean is within the variation of Step 50's
93.243 s mean, so no execution-speed or compile-speed claim is made.

Status: **machine-width constant and identity semantics are complete enough to
remove full-word masked-copy loads before allocation; Linux meaning and tick
count are preserved and generated code is smaller, while measured execution
is unchanged**.

### Step 52: Exact reconstruction recipe-prefix sharing

Inspection of the complete post-allocation MIR found independent reload
recipes at one CFG edge rebuilding the same value prefix.  One concrete edge
loaded `[sim + 33897184]` twice, applied the same 27-bit `and.w32` twice, and
then shifted the two copies by 18 and 9.  This duplication was introduced by
SSA spill reconstruction, not by the inactive interval allocator: every
selected recipe was previously expanded as a separate instruction chain.

Reconstruction now interns exact intermediate recipe prefixes at one concrete
program point or CFG-edge insertion point.  The flat trie is keyed by the
complete `ResolvedBase` and `(prefix VReg, PureStep)`; state-base equality
therefore includes the physical load shape, observed bit range, and exact
MemorySSA snapshot.  Cache lifetime never crosses an insertion point or edge.
The final result of every recipe is deliberately excluded so each logical
reload retains a distinct SSA representative.  Expected time is linear in the
number of recipe steps, and extra storage is bounded by unique intermediate
prefixes at the current insertion point rather than the whole function.

The concrete MIR is now one `load`, one `and.w32`, and two shifts.  Emitted x86
likewise changes from two loads and two masks to one load/mask, a register copy,
and two shifts; the associated edge no longer needs the two `xchg` instructions
which arranged the independently materialized results.  Focused tests prove
common-prefix sharing and distinct final definitions.  All 294 register-
allocation tests and all 1002 library tests pass.  Dynamic-NBA tests pass 33
with one upstream ignore, cross-block NBA tests pass 11 with one ignore,
flip-flop tests pass 200 with 42 ignores, native execution passes 16/16,
native testbench passes 60 with one ignore, and counter passes 9 with three
Veryl ignores.  Package all-target check, strict clippy, and format gates pass.

The complete trace is at
`target/heliodor/analysis/step72-shared-recipe-prefixes`.  All three SIR dumps
are byte-identical to Step 51.  Full MIR falls from 52,213,544 to 52,212,355
bytes.  The concrete `eval_comb` x86 body ends at `0x87718` instead of
`0x87726`, while its 5,352-byte spill frame and the fused `0x1500` prologue
remain unchanged.

Both trace-free non-LTO runs reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`.  They compiled in 80.720 and 80.216 s and executed in
92.850 and 93.337 s.  The 93.093 s execution mean is only 0.6% below Step 51's
93.661 s mean, so no runtime-speed or compile-speed claim is made.

Status: **exact duplicate reload-recipe prefixes at one insertion point are
shared without weakening MemorySSA identity or logical SSA ownership; Linux
meaning and tick count are preserved, but this local repair does not address
the dominant throughput gap**.

### Step 53: MemorySSA-proved demanded-prefix state forwarding

Inspection of the complete scheduled and post-allocation MIR found direct
state traffic such as `store.i32 [sim + 33897184]` followed by a same-address
`load.i64`, even though every SSA user immediately discarded all but the low
27 bits.  The load was therefore not required by RTL semantics: the reaching
32-bit store already established every bit any consumer could observe.

Late state forwarding now computes an all-users low-prefix demand for direct
SimState loads and asks the shared MemorySSA for the definition reaching that
exact byte prefix.  It forwards only when one dominating direct store starts
at the same address and covers the complete demanded prefix.  A full-width or
unsupported user, phi use, MemoryPhi at a CFG join, unknown write, or later
overlapping partial write retains the original load.  Exact same-shaped
round-trip forwarding still takes precedence.  The pass remains after
pressure scheduling, so it does not reorder the scheduler's memory effects.

In the concrete hot path, post-scheduling MIR changes from a 64-bit state load
plus 27-bit mask to applying that mask directly to the reaching store source.
Final x86 changes from `mov r8,[r15+2053AE0h]` followed later by
`and esi,7FFFFFFh` to a register copy from the value already stored in `ebx`.
Pressure-selected edge rematerializations that remain use a 32-bit state load,
not the unneeded 64-bit load.

Focused tests cover same-block and cross-block forwarding, a full-width user,
an intervening partial write, and a one-arm-store MemoryPhi join.  All 299
register-allocation tests and all 1007 library tests pass.  Dynamic-NBA tests
pass 33 with one ignore, cross-block NBA tests pass 11 with one ignore,
flip-flop tests pass 200 with 42 ignores, native execution passes 16/16,
native testbench passes 60 with one ignore, and counter passes 9 with three
ignores.  Package all-target check, strict clippy, and format gates pass.

The complete trace is at
`target/heliodor/analysis/step73-width-aware-state-forwarding`.  All three SIR
dumps are byte-identical to Step 52.  Full MIR falls from 52,212,355 to
52,178,634 bytes.  The raw spill frames fall from 5,352 to 5,344 bytes for
`eval_comb` and from 5,368 to 5,360 bytes for the fused function; their x86
stack allocations fall from `0x14f0` to `0x14e0` and from `0x1500` to `0x14f0`
after alignment.

Both trace-free non-LTO runs reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`.  They compiled in 81.079 and 81.077 s and executed in
96.678 and 93.437 s.  The 95.058 s execution mean is above Step 52's 93.093 s
mean and within the historical run-to-run spread, so no execution-speed or
compile-speed claim is made.

Status: **a wider physical-state reload is removed only when an all-users
width proof and the exact MemorySSA reaching definition make it redundant;
Linux meaning and tick count are preserved, generated MIR and spill frames
are smaller, but the dominant throughput gap remains open**.

### Step 54: CFG-controlled join arm sinking

The Step 53 hot search loop still loaded a 44-bit result and four one-bit
result fields before selecting between those values and the loop-carried
values.  The branch controlling that selection already existed, so executing
the result loads on the non-matching edge was unnecessary.

Controlled-join elimination now moves a join-local, single-use load or pure
definition DAG to the direct predecessor that actually selects it.  A moved
load must precede the join's first write or memory barrier, and every external
operand must dominate the selected predecessor.  Plans for one join are
validated and published atomically: moved definitions, removed Muxes, join
parameters, and incoming edge arguments cannot expose an intermediate invalid
SSA graph.  The analysis stores one first-effect index per block rather than a
dense instruction-prefix table; recursive work is restricted to the selected
single-use DAGs.

In the concrete loop, all five result loads now occur only in the matching
predecessor.  The non-matching predecessor passes the seven loop-carried values
directly.  Focused tests cover several result loads, a join write that forbids
motion, and a repeated predicate whose actual selected edge differs from an
ancestor branch classification.  All 46 BranchifyMux tests and all 1009
library tests pass.  Dynamic-NBA tests pass 33 with one ignore, cross-block NBA
tests pass 11 with one ignore, flip-flop tests pass 200 with 42 ignores, native
execution passes 16/16, native testbench passes 60 with one ignore, and counter
passes 9 with three ignores.  Package all-target check, strict clippy, format,
and diff gates pass.

The complete trace is at
`target/heliodor/analysis/step74-controlled-join-arm-sinking`.  Pre-optimized
SIR remains 58,711,247 bytes.  Post-optimized SIR falls by 666 bytes, native
SIR by 1,258 bytes, and full MIR by 2,418 bytes.  One trace-free non-LTO run
reached `reboot: Power down` and exactly `cy=9ae070 x3=aa pass=1`; it compiled
in 80.256 s and executed in 93.630 s.  This single sample does not establish a
runtime gain.

A follow-up generic predicate-short-circuit trial was rejected.  It evaluated
all three 9/18/27-bit payload alternatives first, delayed only four field
loads, added a branch, and increased the non-match parallel-copy sequence from
five to six `xchg` instructions.  It still booted with the exact marker, but
executed in 94.515 s, so the trial was removed rather than committed.

Final x86 inspection exposes the larger remaining defect.  A 64-iteration
loop backedge transfers from approximately `0xf0b0` to an edge-copy block near
`0x8718d`, performs five stack stores plus reload/copy work, and jumps back near
`0xeec6`.  Plain RPO places the isolated backedge block at the function tail,
while MIR textual display uses dominance order and obscures that physical
layout.  Phi coalescing must remove the copies; emission layout must also keep
any residual hot backedge block adjacent to its loop.

Status: **match-only result loads are no longer executed on the non-matching
edge and Linux meaning is preserved; measured execution is unchanged, and
loop-phi allocation plus physical block layout are the next large target**.

### Step 55: Post-allocation hot-backedge layout

The Step 54 MIR retained a dedicated copy block for a hot loop backedge.  The
MIR printer displayed blocks in dominance order, but the emitter consumed the
stored RPO.  That physical order placed the copy block at the function tail:
each of the 64 loop iterations took a near conditional jump from approximately
`0xf0b0` to `0x8718d`, executed the edge stores, reloads, and register copies,
then jumped back to approximately `0xeec6`.  This was a layout defect separate
from the still-excessive phi copies themselves.

Emission now identifies a conservative post-allocation backedge chain.  The
chain must start at a branch successor, consist only of phi-free blocks with
one predecessor and an unconditional jump, and eventually jump to a block
which dominates the branch predecessor.  It is moved only in the physical
emission order, immediately after that predecessor; MIR, allocation, edge
identity, and scheduler order are unchanged.  Empty-block label aliasing uses
the same physical order.  If the true successor becomes physically adjacent,
the emitter inverts the condition so that the hot edge is an actual
fall-through.  The layout walk is `O(B + E)` after the shared forward CFG
analysis, with no instruction-sized or pairwise value structure.

In the complete final trace at
`target/heliodor/analysis/step76-loop-edge-layout`, the loop latch is a short
`je` to the exit at `0xf0d2`; its continuation falls directly into the edge
copy block at `0xf0d4`, followed by one jump back to `0xeee8`.  The five stack
stores, state reload, stack reload, and register copies are intentionally still
present.  All pre-, post-, and native-optimized SIR files are byte-identical to
Step 54, and the eval-comb spill frame remains 5,344 bytes.  The emitted
`eval_comb` body is 350 bytes smaller because the remote branch island and its
long transfers disappear.

Focused emitter tests pass 17/17, including exact chain placement and branch
inversion.  The optimized library passes 1011/1011.  Dynamic-NBA tests pass 33
with one ignore, cross-block NBA tests pass 11 with one ignore, flip-flop tests
pass 200 with 42 ignores, native execution passes 16/16, native testbench
passes 60 with one ignore, and counter passes 9 with three ignores.  Workspace
all-target check, package all-target strict clippy, format, and diff gates pass.

Both trace-free non-LTO runs reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`.  They compiled in 81.074 and 80.882 s and executed in
92.371 and 91.962 s.  Both execution samples are below Step 54's 93.630 s, by
1.3% and 1.8%, respectively, but the historical variation is large enough
that this is not claimed as the dominant speed repair.

Status: **a residual hot backedge no longer round-trips through the function
tail; Linux meaning and tick count are preserved and both final samples are
slightly faster, while phi coalescing, stack round trips, and eager predicate
payload evaluation remain the larger targets**.

### Step 56: Selector-disjoint predicate control flow

The Step 55 trace evaluated every payload in a selector sum before testing
which selector value was active.  The concrete predicate had the form
`common && ((kind == 0 && payload0) || (kind == 1 && payload1) ||
(kind == 2 && payload2))`; its 27-, 18-, and 9-bit payload DAGs were therefore
all live at the same time even though at most one could affect the result.
Besides executing unused work, that overlap increased pressure at the join
and amplified its parallel copies.

BranchifyMux now recognizes a bounded selector-disjoint Boolean sum and turns
it into explicit control flow.  It first branches on the common condition,
then tests each distinct constant of the same selector, and evaluates only the
chosen payload.  The analysis takes one execution-unit-wide definition/use
snapshot and publishes at most one plan per original block.  It accepts two
through eight arms and keeps definitions shared by several arms in the head,
so its storage and traversal are linear in the execution unit plus the bounded
moved DAGs rather than combinatorial in possible paths.

The transform is deliberately semantic rather than speculative.  It is
two-state only; each generated branch condition is normalized explicitly with
`ToTwoState`.  The source block may contain only pure immediate, load, unary,
binary, concat, slice, and mux instructions.  A store, commit, event, or other
observable effect rejects the plan.  Every moved definition must be closed to
its branch and may not be used by the retained head, another arm, an edge, or
an external block.  Selector constants must be known and pairwise distinct.

In the Heliodor block around `b135`, the head now loads the common selector
state, then the `kind == 2`, `kind == 1`, and `kind == 0` paths evaluate only
their respective 9-, 18-, and 27-bit payloads.  On the formerly eager false
edge, four stack loads and a five-`xchg` parallel-copy cycle become one stack
load with no such cycle.  The loop backedge falls from five stack stores to two
stores and one `xchg`.  This is an important interaction with phi lowering:
shortening mutually exclusive live ranges removes copies before a later
parallel-copy resolver has to repair them.

The complete trace is retained at
`target/heliodor/analysis/step77b-selector-payload-cfg`.  Pre-optimized SIR is
58,711,247 bytes, post-optimized SIR is 19,610,333 bytes, native-optimized SIR
is 19,547,796 bytes, and MIR is 52,177,456 bytes.  Relative to Step 55, the
extra CFG adds only 348 bytes of post-optimized SIR, 668 bytes of native SIR,
and 1,168 bytes of MIR.  The fused spill frame remains 5,344 bytes.

All 50 BranchifyMux tests pass, including selected-payload placement,
duplicate-selector rejection, branch-condition normalization, and rejection
across a store.  Both trace-free non-LTO runs reached `reboot: Power down` and
exactly `cy=9ae070 x3=aa pass=1`.  Code generation took 80.732 and 80.566 s;
generated-code execution took 92.905 and 91.147 s.  Their 92.026 s mean is
effectively unchanged from Step 55's 92.166 s mean, so no runtime improvement
is assigned to this step.

A similar eager expression around `b144` remains because one of its predicate
values is reused after a join.  Moving that value requires explicit CFG-aware
value placement or duplication; violating the closure proof would change SSA
meaning.  That extension remains separate from this safe first transform.

Status: **mutually exclusive selector payloads execute only on their selected
paths; the concrete join copy cycle and backedge stores shrink while Linux
meaning and tick count remain exact; measured execution is unchanged**.

### Step 57: Constant-work machine-word sign replication

The Step 56 native trace exposed a separate instruction-selection defect that
does not require profiling or aggregate statistics to diagnose.  SIR expresses
a signed value widened to one machine word as a concat of the low suffix and
many repetitions of its one-bit sign.  Native ISel lowered each repeated bit
independently.  For a 32-bit suffix, the emitted sequence began as follows and
continued once for every destination bit through bit 63:

```text
mov r8d, r9d
shr r9, 31
and r9d, 1
mov r10, r9
shl r10, 32
or  r8, r10
mov r10, r9
shl r10, 33
or  r8, r10
...
```

The same value now lowers in constant work:

```text
mov r8d, r9d
shr r9, 31
and r9d, 1
neg r9
shl r9, 32
or  r8, r9
```

The new selector rule is intentionally limited to the native machine-word
boundary.  It requires an exactly 64-bit concat, at least four repetitions of
the same one-bit SIR register in the high part, and one suffix whose width plus
the repetition count is exactly 64.  Negating a canonical one-bit plane makes
the required all-zero or all-one fill; shifting it by the suffix width and
ORing the suffix reconstructs the concat exactly.  If all 64 inputs are the
same bit, only the negation is required.  Four-state execution applies the
same operation independently to the value and unknown-mask planes, preserving
the existing representation rather than inventing width metadata on MIR
virtual registers.  Recognition and lowering are linear in the concat input
list and emit a constant number of MIR instructions.

The complete trace is retained at
`target/heliodor/analysis/step78-repeated-msb-concat`.  Every SIR stage is
byte-identical to Step 56: 58,711,247 bytes before optimization, 19,610,333
bytes after SIR optimization, and 19,547,796 bytes after native SIR
optimization.  MIR falls from 52,177,456 to 51,921,510 bytes.  The end of the
emitted `eval_comb` body moves from approximately `0x86ff7` to `0x8608d`, a
3,946-byte reduction, while its 5,344-byte spill frame is unchanged.  The
same collapse occurs for concrete 16-, 32-, and 48-bit suffix forms found in
the generated workload.

The executable ISel regression covers both two-state and four-state results,
checks the exact `Neg`/`Shl`/`Or` instruction counts, and validates both value
and unknown-mask planes.  All 30 ISel tests and all 1016 library tests pass.
Dynamic-NBA tests pass 33 with one ignore, cross-block NBA tests pass 11 with
one ignore, flip-flop tests pass 200 with 42 ignores, native execution passes
16/16, native testbench passes 60 with one ignore, and counter passes 9 with
three ignores.  Workspace check, package all-target strict clippy, formatting,
and the documentation build pass.

Both trace-free non-LTO runs reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`.  Code generation took 80.348 and 80.444 s;
generated-code execution took 92.280 and 89.330 s.  Their 90.805 s mean is
1.3% below Step 56's mean, but historical variance is too large to assign a
larger claim.  The static defect and instruction reduction are exact; the
remaining runtime gap is still dominated by hot state traffic, overlapping
live ranges, spills, reloads, and residual join copies.

Status: **machine-word sign replication no longer expands one repeated bit at
a time; exact two-state and four-state meaning, Linux tick count, and normal
power-down are preserved; MIR and emitted code shrink materially, with a
modest measured execution improvement**.

### Step 58: Alias-aware cleanup after late state forwarding

The fresh Step 57 trace contained direct evidence that late physical-state
forwarding stopped one pass too early.  Four adjacent packed state bytes had
the same generated shape.  One byte, at `state + 0x214b0`, was updated as:

```text
load byte [state + 0x214b0]
clear bit 1; insert the new bit 1
store byte [state + 0x214b0]
clear bit 4; insert the new bit 4
store byte [state + 0x214b0]
```

There was no read of that address between the stores.  MemorySSA had already
proved the second source-level load equivalent to the first stored value and
late state forwarding had replaced that load with a register copy.  The
ordinary MIR dead-store pass ran before this late transformation, however, so
the newly dead first store reached CSSA, allocation, and machine code.

The local dead-store pass now uses the shared physical-memory effect model
instead of a separate hard-coded alias switch.  In a reverse scan only a read
can make an earlier value observable.  Exact direct reads invalidate only
overlapping stores, a bounded indexed read uses its byte envelope, an unknown
direct read clears only its `SimState` or `StackFrame` domain, and indirect
runtime memory remains disjoint.  Pure writes are not observation barriers;
read-modify-write instructions retain their read effects.  The pass is run a
second time immediately after late state forwarding and before CSSA.

Tracked direct stores use one ordered offset map per direct base and a
four-bit width set per start offset.  Exact-store lookup and insertion are
`O(log s)`.  A read which overlaps `k` tracked starts is
`O(log s + k log s)` and block-local storage is `O(s)`.  Ordinary one-, two-,
four-, and eight-byte reads examine only starts within seven bytes of their
range.  This replaces the old HashMap-wide retain on every store, whose
store-only worst case was quadratic, without constructing a whole-function
alias graph.

The complete candidate trace is retained at
`target/heliodor/analysis/step85-late-dse`.  All SIR stages remain identical to
the authoritative Step 57 HEAD trace.  Full MIR falls from 51,973,923 to
51,521,543 bytes.  At the concrete `0x214b0`, `0x2f270`, `0x21570`, and
`0x2f2e0` updates, both bit inserts now stay in a register and only the final
byte is stored.  The parent A trace at
`target/heliodor/analysis/step85-ab-a-head` is byte-identical to the earlier
HEAD trace through complete MIR, so the paired execution did not compare a
different baseline program.

The focused MIR optimization suite passes 70/70, late state promotion passes
15/15, and the complete library suite passes 1020/1020.  Every trace-free run
reached `reboot: Power down` and exactly `cy=9ae070 x3=aa pass=1`.  The adjacent
parent/candidate pair compiled in 73.919/74.250 s and executed in
91.640/91.492 s.  Two earlier candidate executions were 90.925 and 94.953 s,
so no generated-code throughput improvement is assigned to this step.

Status: **MemorySSA-exposed intermediate packed-state stores are removed with
byte-range alias precision and bounded local storage; exact Linux meaning and
tick count are preserved; emitted MIR shrinks, while measured execution is
unchanged**.

### Step 59: Constant-work repeated-bit chunks in wide concat lowering

Step 58 still contained two complete 64-step shift/OR ladders in one native
MIR block.  They did not originate in the machine-word concat handled by Step
57.  The optimized SIR represented a 128-bit signed operand as a wide concat
whose high chunk contained 64 copies of the same sign bit.  The generic wide
concat repacker flattened those copies into independent one-bit pieces and
rebuilt the high word literally:

```text
sign = shr source, 63
t1 = shl sign, 1
a1 = or sign, t1
t2 = shl sign, 2
a2 = or a1, t2
...
t63 = shl sign, 63
high = or a62, t63
```

The wide repacker now coalesces each adjacent run of at least four references
to the same canonical one-bit VReg.  One `neg` turns that bit into an all-zero
or all-one machine word, after which the ordinary chunk packer can consume any
part of the fill.  Runs spanning more than one word are represented by
at-most-64-bit views of the same fill.  Value and four-state mask planes use
the same lowering, as do the wide concat arms used by mux chunk blending.

The run compaction reuses the existing flat-part vector in place; its output
cannot contain more entries than its input.  Repacking consumes every compact
part once and emits each destination chunk once, so time is
`O(parts + chunks)` and auxiliary storage beyond the already required part and
chunk vectors is constant.  The common packer also starts each chunk from its
first real piece instead of manufacturing a zero followed by `or zero`, while
preserving masking and cross-chunk extraction for partial pieces.

The executable regression builds `Concat([sign x 64, low64])`, requires one
`neg` and no concat `shl` per two-state/four-state plane, then JIT-executes the
128-bit store and checks both value words and both unknown-mask words.  It
failed before the change with 63 shifts, 64 ORs, and a width-normalizing move
for every repeated input.  All 31 native ISel tests and all 1021 library tests
pass after the change.

The complete trace is retained at
`target/heliodor/analysis/step86-wide-sign-fill`.  Pre-optimized,
post-optimized, and native-optimized SIR are byte-identical to Step 58.  The
concrete ladders now read simply as `shr sign, 63; neg sign`, and full MIR falls
from 51,521,543 to 51,245,987 bytes.  The trace-free non-LTO run reached
`reboot: Power down` and exactly `cy=9ae070 x3=aa pass=1`; compilation took
73.399 s and execution took 91.489 s.  Step 58's adjacent retained candidate
executed in 91.492 s, so this large static reduction does not improve the
Linux workload: the affected wide arithmetic arm is not a dominant executed
path.

Status: **wide repeated-bit chunks lower in constant work with exact
two-state/four-state semantics and linear bounded lowering; Linux meaning and
tick count remain exact; MIR shrinks, while measured execution is unchanged**.

### Step 60: Allocator-visible sparse-commit scratch clobbers

The emitted standalone `apply_ff` body still hid every register used by each
`SparseCommit` loop from register allocation.  At the first concrete commit,
the empty-bitmap path was:

```text
push rax; push rcx; push rdx; push rsi; push rdi; push r8; push r9
mov rax, [summary]
mov [summary], 0
test rax, rax
je done
...
done:
pop r9; pop r8; pop rdi; pop rsi; pop rdx; pop rcx; pop rax
```

Thus a commit with no dirty chunk still performed fourteen hidden stack
operations.  The shared `SparseCommitWorklist` similarly saved all fourteen
allocatable GPRs around its complete loop.  Both pseudos now declare their
fixed scratch sets as ordinary target clobbers.  Existing pressure analysis,
spill planning, coloring, and allocation verification therefore keep only
genuinely live-through values out of those registers.  Caller-saved scratch
needs no emitter save, while any callee-saved scratch used by the worklist is
included once in the function prologue and epilogue.

Removing the local saves exposed an existing native-label contract: the final
internal loop label and the following fall-through MIR block occupied the same
machine instruction.  The sparse emitters now bind that terminal label using
the continuation block's label, as the branchy select emitters already do,
instead of adding a nop or retaining a semantically unnecessary pop.  Split
fall-through regressions execute a value live across each pseudo and verify
both the clobber result and the one-label layout.  Zero-capacity pseudos are
also treated as true no-code instructions by allocation and layout.

Clobber discovery adds only a fixed seven- or fourteen-bit register mask at
each pseudo.  Callee-save discovery scans MIR instructions once, so the added
time is `O(instructions)` and auxiliary storage remains one fixed register
set.  No pairwise live-range relation or instruction-sized side table is
introduced.

The complete candidate trace is retained at
`target/heliodor/analysis/step88-sparse-commit-clobbers`.  All three SIR stages
are byte-identical to Step 59, including across two independently generated
candidate traces.  Complete MIR changes from 51,245,987 to 51,199,426 bytes.
At `apply_ff` offset `0x5d`, the summary load now follows the six initial state
copies directly; the seven pushes and seven matching pops are absent.  Later
commits have the same form.  The allocator chose `rbx`, `rbp`, and `r12` for
live-through values and saves them once at function entry; the spill frame
remains zero bytes.

Both focused executable clobber/fall-through tests and all 1023 library tests
pass.  The trace-free non-LTO Linux run reached `reboot: Power down` and
exactly `cy=9ae070 x3=aa pass=1`; compilation took 73.593 s and execution took
92.635 s.  The earlier worklist-only run executed in 92.028 s, and Step 59 in
91.489 s, so no Heliodor throughput improvement is assigned to this change.
The hot fused scheduler path does not repeatedly execute the standalone
per-region commit sequence; the structural defect is fixed, but the observed
Linux gap remains in the combined body.

Status: **sparse pseudo scratch ownership is explicit and verified; hidden
per-commit stack saves are removed without replacement spills; exact Linux
meaning and tick count are preserved; measured Heliodor execution is
unchanged**.

### Step 61: Recover full-domain indexed FF stores

The Step 60 SIR still represented one register-file write as a linear chain of
exact selector comparisons.  The floating-point PRF had 64 stages and the
integer PRF repeated the same 64-stage shape for four write ports.  Every
stage branched to one static element Store or an empty arm and reconverged
before testing the next selector value.  This is an O(element-count) lowered
form of one O(1) unpacked-array write, not an instruction-selection problem.

`IndexedStoreRecoveryPass` now recovers the source operation before native
lowering.  It uses the complete shared `SirCfg` and requires all of the
following: two-state execution; a declared unpacked-array shape; exact coverage
of the selector's complete domain; key `k` writing static element `k`; one
unobserved Store per selected arm; empty non-selected arms; exclusive joins;
and alpha-equivalent pure value DAGs.  Definitions removed with the ladder may
not escape it.  Only after the complete proof succeeds does the pass replace
the chain with one `SIROffset::Element` Store.  The production default is also
exposed as the `indexed_store_recovery` `SirPass`, so pass-disable diagnostics
can reproduce the unoptimized pipeline exactly.

All disjoint ladders are collected from one CFG snapshot, prepared
transactionally, rewritten together, and followed by one reachability/DCE
cleanup.  Exact constant results are memoized.  CFG construction, stage
indexing, chain validation, and rewrite storage are therefore linear in the
execution unit and the removed arm DAGs; there is no repeated whole-CFG scan,
path enumeration, selector-domain expansion, or size cap.

The first candidate exposed a separate state-publication defect.  A dynamic
WORKING access was rejected unconditionally by the direct round-trip rewrite,
so native lowering copied the complete 512-byte PRF from STABLE to WORKING,
performed one indexed Store, and copied all 512 bytes back.  That version still
booted correctly but regressed execution to 96.533 s.  Dynamic data access is
not itself a semantic barrier.  The rewrite now rejects dynamically addressed
seed/apply Commits, while ordinary dynamic Loads and Stores are governed by
the existing complete-CFG memory-dependence proof.  An Element access without
a dynamic intra-element offset gets a finite conservative range from its SIR
selector width and element width.  Old-STABLE observations, competing writes,
and exits before publication still reject direct publication.

The final complete trace is retained at
`target/heliodor/analysis/step93-indexed-store-cli`.  Its pre-optimized SIR
is 58,711,247 bytes, post-optimized SIR is 18,663,178 bytes, native-optimized
SIR is 18,841,073 bytes, and MIR is 50,095,656 bytes.  Relative to Step 60,
post-optimized SIR falls by 946,380 bytes and MIR by 1,103,770 bytes.  The PRF
hot path is now one direct
`store.i64 [sim + 186400 + index]` or
`store.i64 [sim + 187552 + index]`; its 512-byte copy-out is absent.  Complete
MIR `memcopy` occurrences fall from 72 to 36.  The sequential Step 91,
batch-rewrite Step 92, and final CLI-addressable Step 93 are byte-identical at
every retained SIR stage and in complete MIR.

The seven focused recovery/option tests include exhaustive selector-result
comparison, incomplete-domain and observable-effect rejection, four-state
rejection, two independent ladders recovered from one CFG analysis, and
CLI/default and explicit-disable behavior.  Seven working round-trip tests and
seven commit-hazard tests pass.  The common gates pass all
1,033 library tests, 60 native-testbench tests with one documented ignore, and
9 counter tests with three documented ignores.  Package check, all-target
strict clippy, formatting, and diff checks pass.

Both final trace-free non-LTO runs reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`.  Code generation took 62.845 and 63.080 s; generated
code execution took 93.248 and 92.955 s.  The 93.102 s mean is within the
existing variation around Step 60's 92.635 s, so no throughput improvement is
assigned to this step.  The static O(N) defect and accidental whole-object
round trip are removed, but they are not the dominant remaining Linux path.

Status: **full-domain FF write ladders recover to direct indexed state stores
with complete CFG and alias proofs; exact Linux meaning and tick count are
preserved; SIR/MIR shrink materially, while measured execution is unchanged**.

### Step 62: Lazy selector arms across value-producing CFG regions

Inspection of the complete optimized SIR exposed a case-dispatch defect which
was larger than the remaining phi-copy cycles.  The cross-block priority
rewrite moved selector conditions into a branch spine but left each selected
value in the dominating block.  ScheduleLate then treated an edge argument as
a use in its predecessor, so it could move an arm DAG only as far as the
decision block.  Opcode 28 and later therefore computed their shifts, adds,
bitwise operations, and state projections before testing that opcode.  More
seriously, four guarded 32-bit `DivS`, `DivU`, `RemS`, and `RemU` regions ran
before the selector reached opcodes 24--27.

`CrossBlockPriorityChainPlan` now owns the closed single-use pure DAG of every
case arm as well as the condition DAGs.  It emits explicit selected leaves and
moves each owned DAG directly into its leaf.  This preserves the source Mux
priority while making the payload control-dependent instead of merely
replacing a Mux with a branch after the same eager work.

The division results cross block parameters, so instruction-only placement
cannot move them.  `GuardedRegionSinkingPass` now recognizes a pure
single-result diamond whose parameter is used in one later
control-dependent block.  It bypasses the original diamond and recreates the
same guard, arm instructions, and result merge inside that selected block.
The divide-by-zero branch is moved intact; no speculative division or changed
RTL arithmetic semantics is introduced.  Serial diamonds may share only a
merge/head boundary, allowing the four word operations to be planned from one
CFG snapshot.  Arm blocks and destination leaves remain disjoint.

Both analyses are sparse.  Use collection, predecessor validation,
dominance/postdominance checks, and arm-closure walks are linear in CFG size
and def-use edges.  There is no path enumeration, block-by-value matrix, or
selector-domain expansion.  Cyclic regions, effectful arms or destinations,
multiple result parameters, escaping result uses, and ambiguous predecessor
sets are rejected.

The final complete trace is retained at
`target/heliodor/analysis/step97-lazy-selector-arms-final`.  Its pre-optimized
SIR is 58,711,247 bytes, post-optimized SIR is 18,667,791 bytes,
native-optimized SIR is 18,888,369 bytes, and MIR is 50,085,749 bytes.  It is
byte-identical to the independently generated Step 96 trace at all four
stages.  In both optimized SIR stages, opcode 23's multiply and every later
payload execute in their selected leaf.  The four word divide/remainder
instructions occur below their opcode tests and retain their original
zero-divisor diamonds.  Final x86 likewise performs the selector comparison
spine before entering the corresponding `div`/`idiv` target.

Focused tests cover payload placement for a cross-block priority chain, one
guarded divide moved beneath a selected use, and four-shaped serial guarded
regions transformed atomically.  All 1,035 library tests pass, along with 60
native-testbench tests with one documented ignore and 9 counter tests with
three documented ignores.

Both trace-free non-LTO runs reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`.  Code generation took 62.985 and 65.635 s; generated
code execution took 90.748 and 96.790 s.  Their 93.769 s mean does not improve
on Step 61's 93.102 s mean within the observed variance.  The eager-arm defect
is removed and final MIR is 9,907 bytes smaller, but these particular ALU arms
are not the dominant remaining Linux path.

Status: **selector payloads and guarded value CFGs execute only in their
selected arms; exact Linux meaning and tick count are preserved; measured
Heliodor execution is unchanged, so the dominant throughput gap remains
open**.

### Step 63: Loop-backedge phi affinity through CSSA snapshots

The complete Step 62 x86 still paid parallel-copy permutations on repeated
loop edges.  The concrete fused-function loop near `0x279` has seven header
phis.  Its entry values already occupied the header registers when the header
was colored, while the backedge values did not yet have colors.  CSSA had also
isolated the interfering backedge sources as
`snapshot = mov.w64 source`.  The existing affinity graph therefore contained
`source <-> snapshot <-> header phi`, but the dominance-order streaming
colorer could see only `snapshot <-> header phi` after `source` had already
been colored.  It matched the one-time entry edge and repaired the 64-times
repeated backedge with two `xchg` instructions on every continuation.

The colorer now contracts that exact `Mov` node for affinity purposes when,
and only when, the phi belongs to a natural-loop header and its predecessor is
inside that same loop.  The original source receives a soft preference for
the already-colored header result.  Register liveness, interference,
fixed-register constraints, and forbidden colors remain authoritative, and
the MIR instruction and CFG order are unchanged.  Ordinary joins are not
contracted.  Construction scans the VReg domain, MIR instructions, and phi
rows once, using one optional source entry per VReg; time and storage are
`O(V + I + P)` with no live-set matrix or path enumeration.

Two broader trials were rejected before this final form.  Contracting exact
copies at every join moved copy cycles into unrelated one-shot CFG arms.
Applying the loop preference only to phi bundles left scalar backedge updates
in their old colors and recreated the same edge permutations.  Neither trial
is retained.  The final implementation also preserves one affinity vote per
CFG edge rather than deduplicating equal VReg neighbors.  In the accepted
trace, the seven concrete backedge sources and header results use identical
registers.  The old sequence at `0x3e0` containing two `xchg` instructions is
absent.  Across `eval_comb`, `xchg` falls from 1,168 to 1,119; across the fused
function it falls from 915 to 860.

The complete accepted trace is retained at
`target/heliodor/analysis/step98e-loop-backedge-affinity-final`.  All three SIR
files and all MIR through pressure scheduling are byte-identical to Step 62.
Their SIR sizes remain 58,711,247, 18,667,791, and 18,888,369 bytes.  Physical
assignment and out-of-SSA code change; the complete MIR trace falls from
50,085,749 to 50,077,640 bytes.

Focused color tests cover both a loop-backedge snapshot which must expose its
source affinity and an ordinary join which must remain uncontracted.  All 302
register-allocation tests and all 1,037 library tests pass.  Native testbench
passes 60 tests with one documented ignore, counter passes 9 with three
documented ignores, and all-target check, strict Clippy, formatting, and diff
checks pass.

Both trace-free non-LTO runs reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`.  Code generation took 63.032 and 62.776 s;
generated-code execution took 89.473 and 90.408 s.  Their 89.941 s execution
mean is 3.829 s, or 4.1%, below Step 62's 93.769 s mean.  This is a measured
improvement from repeated-edge register placement; it does not yet address
the remaining stack reloads or non-loop parallel copies.

Status: **CSSA no longer hides an available natural-loop backedge affinity;
the concrete repeated phi permutation is removed, exact Linux meaning and
tick count are preserved, and two non-LTO runs average 4.1% faster than the
immediate parent**.

### Step 64: Convergent machine-interval spilling and sparse split queries

The opt-in interval allocator could not previously compile the Linux input.
At `bb5937`, spilling allocator-created values which fed a machine phi inserted
one reload for every incoming phi source. Fourteen such reload results were
then simultaneously live until the same edge; the next ordinary operand made
fifteen fixed ranges compete for fourteen allocatable GPRs. Further splitting
could not change that pressure, so allocation ended with
`JOINT_ALLOC.UNSPLITTABLE_PRESSURE`.

Machine-interval spilling now leaves phi-edge sources in their conventional
stack homes. Semantic source phis retain their immutable source-MIR identity;
strict-SSA phis created by SplitEditor retain the synthetic VReg consumed by
out-of-SSA. Both forms are represented as exact edge locations, participate
in sparse stack-slot interference, and lower atomically without a fake reload
live range. Instruction uses still receive one-use reloads. This removes the
impossible edge pressure without weakening register interference or changing
the source CFG.

The first successful Linux-sized run then exposed two independent quadratic
implementation costs. Every edit rebuilt the complete pending allocation heap
and rescanned all register-region metadata, while every candidate cut
enumerated every original instruction boundary and repeatedly linearly scanned
the value's sparse live segments. Pending heap entries are now invalidated
lazily, active regions have an inverse VReg index, live segments are found by
block with binary search, and the latest source boundary before a stable cut is
computed directly from its immutable zone. A split query is now `O(log B)` in
the number of live blocks for the value plus `O(1)` boundary selection, instead
of `O(B * I_block)`; local publication touches only changed values and regions.

On the same `heliodor-dev`, non-LTO Linux input, `eval_comb` split selection
fell from 114.623 s to 0.272 s and edit time from 144.299 s to 43.459 s. Its
joint reallocation fell from about 357 s to 132.788 s. The fused function,
which previously did not finish within 900 s, completed joint reallocation in
247.084 s and total interval allocation in 284.368 s. The timed compile-only
run completed in 330.241 s; the independent trace-free full run compiled in
332.005 s.

All 304 register-allocation tests and all 1,039 library tests pass. The full
run reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`; generated-code execution took 103.855 s. That is
15.5% slower than Step 63's 89.941 s mean, so the interval allocator remains
opt-in and no execution-throughput improvement is claimed. Its remaining
structural defect is explicit: the fused function still performs 48,456 split
edits and restarts allocation 61,187 times, producing code inferior to the
production SSA allocator.

Status: **machine-phi spills are represented as edge stack locations, the
interval allocator now converges on the full Linux workload, and its dominant
compile-time scans are removed; generated-code quality and repeated one-cut
allocation remain open**.

### Step 65: Productive free prefixes and transactional stack-home definitions

The interval allocator previously chose a physical-register frontier for one
candidate by copy count and then by register number. It did not compare how
much of the candidate's live range each equally cheap frontier retained. A
register blocked immediately after the definition could therefore beat one
which remained free through many real uses. The resulting tiny fragments were
then copied, requeued, and split again.

Frontier construction now records the number of retained machine uses and the
exact stable-slot length reached by each register's maximal free prefix. For
frontiers of the same candidate, selection first maximizes retained uses per
inserted copy and then retained length per copy; cross-candidate spill-density
ordering is unchanged. Coverage is computed by one sparse CFG traversal over
the candidate interval. Epoch-marked block bounds reuse the existing workspace
without clearing a block-sized table for each register.

The larger retained prefixes exposed two pre-existing stack-home transaction
bugs. A later machine spill store used to append at the end of an original
definition's stable anchor zone. If an older allocator-owned use already
occupied that zone, the rewritten reload could precede its defining store.
Store placement now uses the first existing same-block use as an upper boundary
only when the default append point would cross that use. This preserves all
actual def/use constraints without imposing source order on independent RTL
operations. When a later strict-SSA split inserts a copy before a fixed stack
store, `AllocationIr` now returns the exact store owner and the expanded
stack-home row is retargeted in the same transaction. The update is `O(1)` by
dense `StackHomeId`; it does not scan all homes or pin the store to the original
VReg. A dead stack-defined phi with no remaining reload now consumes neither an
out-of-SSA copy nor a frame slot.

On the same non-LTO `heliodor-dev` Linux workload, `eval_comb` split edits fell
from 41,722 to 22,946 and allocator calls from 53,223 to 34,493. The fused
function fell from 48,456 to 26,392 split edits and from 61,187 to 39,285 calls.
Compile-only completed in 228.722 s, versus 330.241 s at Step 64. An independent
trace-free full run compiled in 228.524 s and executed in 101.035 s. It reached
`reboot: Power down` and exactly `cy=9ae070 x3=aa pass=1`. Execution is 2.7%
below Step 64's 103.855 s, but still 12.3% above the production Step 63 mean of
89.941 s, so the interval allocator remains opt-in.

All 307 register-allocation tests and all 1,042 library tests pass, including
regressions for same-zone store/reload order, exact stack-store owner reporting,
stack-home metadata retargeting, dead phi homes, and productive frontier
choice. Workspace all-target check, package all-target strict Clippy,
formatting, diff checks, and the clean Heliodor source check pass. Workspace-wide
strict Clippy additionally reaches an unrelated pre-existing Rust 1.97
`explicit_counter_loop` lint in `celox-wasm/src/lib.rs`; that file is unchanged
by this step. Complete candidate MIR inspection remains the next step rather
than being replaced by aggregate counters.

Status: **frontier choice now retains useful live-range prefixes instead of
winning ties by register number; Linux compile time drops by about 31% and the
generated program is modestly faster, while the remaining execution gap and
edge-copy quality remain open**.

### Step 66: Close trivial MIR values before and after allocation

Complete post-allocation MIR inspection exposed predicates whose selected
values were identical, machine-width identities created after the last
algebraic pass, unused phi chains retained by instruction-only DCE, and dead
rematerializations introduced during allocation. These are not harmless
emitter peepholes: retaining their definitions gives the predicate graph live
ranges, CSSA snapshots, phi copies, and spill homes before the emitter finally
discovers that the selected physical register is unchanged.

The final pre-allocation pipeline now folds equal-value `Select`, `CmpSelect`,
`CmpImmSelect`, and `GuardedCmpSelect`, reruns algebraic simplification after
immediate-form lowering, propagates the resulting copies, and removes unused
phis to a fixed cascading DCE boundary. A separate post-allocation cleanup
folds equal split representatives and removes dead instructions without copy
propagation. It deliberately preserves phi rows because the verified
parallel-copy plan has already been constructed at that phase boundary. The
first Linux-sized trace caught that distinction: removing a post-allocation
phi correctly failed plan verification, after which instruction-only cleanup
completed on the same input.

All three SIR dumps are byte-identical to Step 65. The full interval-allocator
MIR shrank from 53,013,387 to 52,042,443 bytes and from 1,906,113 to 1,877,876
lines. No equal-value select or self-conditional-move remains in the complete
trace. The `eval_comb` spill frame fell from 6,184 to 5,696 bytes and its
emitted body from 630,288 to 618,645 bytes; the fused frame fell from 6,208 to
5,744 bytes and its body from 860,533 to 848,332 bytes. Immediate stack
store/reload transitions remain visible and are the next allocator defect.

The focused MIR suite passes 73/73, the library suite 1,045/1,045, native
testbench 60 passed with one ignored, and counter 9 passed with three ignored;
the package check, formatting, and clean Heliodor source checks pass. The
trace-only build took 227.768 s. An independent trace-free full run compiled
in 228.244 s, reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`, and executed in 102.749 s. That single execution
sample is above Step 65's 101.035 s and therefore establishes semantics and
structural removal only, not a throughput improvement.

Status: **dead predicate and identity graphs no longer enter allocation, dead
phis cascade away before CSSA, and post-allocation cleanup respects the frozen
parallel-copy plan; immediate split-to-stack transitions remain open**.

### Step 67: Forward adjacent allocator-home reloads

Complete post-allocation MIR inspection showed two forms of reload which the
interval allocator introduced between otherwise adjacent operations. A
machine value was stored to its private stack home and immediately reloaded
for its first use. More broadly, a value already present in a register was
stored to one packed-state word, reloaded through a MemorySSA state recipe,
and then stored to a second word. The scheduled MIR used the same register for
both state stores; only home selection introduced the intervening load.

Atomic allocation lowering now performs a bounded post-color forwarding pass.
A stack reload is replaced only when allocation IR records the immediately
preceding store with the same `StackHomeId`. A deferred-state reload or direct
state-recipe leaf is replaced only when its materialized direct load is
immediately preceded by an exact `SimState` store with the same offset and
width. Narrow state values use an 8/16-bit mask or a 32-bit move so the
zero-extension produced by the original load is retained. Stores remain in
place for later reloads and observable state. No CFG search, physical stack
offset inference, or RTL source rewrite is involved. The pass is linear in
allocation instructions and retains replacement rows for only one block at a
time. The completed physical assignment and SSA-destruction plan are rebuilt
and independently verified after forwarding.

For example, the final interval MIR changed from
`store.i64 [sim + 182496]; load.i64 [sim + 182496]; store.i64 [sim + 183728]`
back to the scheduled dataflow `store; mov; store`; a later non-adjacent load
from the same state remains. All three SIR dumps are byte-identical to Step 66.
The full MIR fell from 52,042,443 to 52,031,555 bytes and from 1,877,876 to
1,877,701 lines. The `eval_comb` emitted body fell from 618,645 to 617,892
bytes and the fused body from 848,332 to 847,529 bytes; spill frames are
unchanged.

The two focused home-forwarding regressions pass, including narrow state
canonicalization. The library suite passes 1,047/1,047, native testbench 60
passed with one ignored, counter 9 passed with three ignored, and package check
and formatting pass. The final trace took 227.839 s. A trace-free run compiled
in 226.362 s, reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`, and executed in 106.059 s. This is a proved memory
traffic and code-size reduction, but the execution sample is slower than the
previous samples, so no throughput gain is claimed. Wider load normalization
and phi/coalescing quality remain larger targets.

Status: **adjacent stack and MemorySSA-state reloads no longer reread a value
which is still in a register; semantics and allocation remain independently
verified, while the measured generated-code gap is unchanged**.

### Step 68: Select widened whole-variable loads at physical width

Complete initial MIR inspection showed that a SIR load widened to a 64-bit
result could still name a 32-bit variable in the physical state layout. ISel
first emitted `load.i64` into a transient VReg and then `mov.w32` into the SIR
result. Besides the redundant instruction and wider memory access, this
created another allocator value whose live range existed only to perform the
zero extension already provided by an x86-64 32-bit load.

Two-state whole-variable loads now use the exact native 8/16/32-bit physical
access when the memory layout proves that access covers the complete variable.
The load defines the original SIR result VReg directly and its state-home
recipe records the physical width. This is not a general VReg-width
inference: non-native widths such as 27 bits retain the explicit load-and-mask
sequence, and four-state values retain their separate value/mask handling.
The concrete Heliodor sequence changed from
`v50204 = load.i64 [sim + 33900268]; v43843 = mov.w32 v50204` to
`v43843 = load.i32 [sim + 33900268]` in initial MIR.

All three SIR dumps are byte-identical to Step 67. The complete interval MIR
lost 127 lines, while its textual dump grew by 9,582 bytes because the smaller
pre-allocation graph perturbed later VReg assignment. The `eval_comb` and
fused spill frames each fell by 40 bytes, to 5,656 and 5,704 bytes. The emitted
`eval_comb` body grew by 847 bytes while the fused body fell by 403 bytes;
across all five emitted bodies the net change is a 355-byte increase. This is
direct evidence that allocator/coalescing instability can outweigh a locally
strictly smaller value graph, rather than evidence for retaining the
redundant load.

The two focused ISel regressions pass, including the non-native-width guard.
The library suite passes 1,049/1,049, native testbench 60 passed with one
ignored, counter 9 passed with three ignored, and package/workspace checks,
package strict Clippy, formatting, diff, and clean Heliodor source checks pass.
The final trace took 230.028 s. A trace-free run compiled in 226.570 s, reached
`reboot: Power down` and exactly `cy=9ae070 x3=aa pass=1`, and executed in
102.671 s. The single execution sample establishes semantics only; no
throughput gain is claimed.

Status: **widened native state loads no longer create a redundant transient
and truncating copy; the resulting allocation perturbation reinforces that
stable phi affinity and coalescing, rather than more local peepholes, are the
next code-quality problem**.

### Step 69: Reuse physically available state loads after allocation

Complete post-allocation MIR inspection exposed a larger consequence of CSSA
phi isolation than the remaining parallel-copy cycles.  Several interfering
phi rows correctly owned distinct edge snapshots, but those snapshots carried
the same exact packed-state reconstruction recipe.  The interval allocator
materialized every snapshot independently even when the first load's physical
register still held the value.  One concrete generated sequence therefore
read the same byte seven times before seven conditional selects.

Allocation lowering now performs exact, block-local value availability over
the completed physical assignment.  A direct `SimState` load is reused only
while an assigned physical register still contains a load of the same offset
and machine width.  Any definition or target clobber kills that register;
overlapping or unknown state writes kill the corresponding memory value; and
availability never crosses a block boundary.  When the destination register
already contains the value it is preferred, otherwise copies fan out from the
earliest surviving load instead of forming a serial copy chain.  The same
bounded pass runs after the final post-RA copy-folding peephole because that
peephole can recreate independent loads.  The exact assignment is verified
again after the late rewrite and the frozen SSA-destruction plan is still
verified immediately before emission.  The pass uses at most the 14
allocatable registers per block: `O(instructions * 14)` time and `O(14)`
additional space, with no new RTL ordering rule or global live-range growth.

The accepted parent trace is
`target/heliodor/analysis/step113-e7c2449d-baseline`; the candidate is
`target/heliodor/analysis/step112-state-load-cse-final`.  Their complete
pre-optimized, post-optimized, and native-optimized SIR files are byte-identical
with hashes `51b1befa...`, `7c19aec1...`, and `fe40b3d4...`.  The complete MIR
falls from 52,037,515 to 51,848,787 bytes and from 1,877,983 to 1,874,850
lines.  Spill frames remain 5,656 and 5,704 bytes.  Emitted `eval_comb`-class
code falls by 14,518 bytes and the fused body by 15,321 bytes; the five emitted
bodies shrink by 29,960 bytes in total.  In the inspected byte-load sequence,
seven `movzx` instructions from `[r15+206C4A3h]` become one load followed by
uses of the surviving register; a later load in a distinct value interval
remains.

Five focused regressions cover exact reuse, overlapping-state invalidation,
physical-register redefinition, post-RA copy-fold ordering, and destination
register preference.  The library suite passes 1,054/1,054, native testbench
60 passed with one ignored, counter 9 passed with three ignored, and package
and workspace checks, package all-target strict Clippy, formatting, diff, and
clean Heliodor source checks pass.  The trace-free non-LTO interval-allocator
run compiled in 346.444 s, reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`, and executed in 100.155 s.  This establishes exact
semantics and a roughly 30 KiB machine-code reduction; the single execution
sample does not establish a throughput improvement.

Status: **CSSA snapshots retain distinct SSA ownership without independently
reloading an identical physically available state value; exact invalidation,
assignment, and Linux semantics are verified, while broader phi coalescing and
spill placement remain open**.

### Step 70: Reuse physically available private stack reloads

Complete post-allocation MIR still reread an unchanged private stack home in
the same basic block.  One concrete sequence loaded `[sp + 40]` separately for
an add and a following subtract even though the first loaded physical value
survived.  This is the non-adjacent case which Step 67 could not see: the store
and first reload need not be adjacent, and ordinary instructions may appear
between two reloads.

The bounded post-allocation availability pass from Step 69 now keys direct
loads by base, offset, and machine width, and therefore handles both
`SimState` and the allocator-owned `StackFrame`.  A physical-register
definition still kills the value in that register.  A direct write kills only
overlapping values of the same base; an unknown direct write kills values of
that base.  Runtime-owned indirect memory is disjoint from both direct bases
under the native memory-effect model.  Availability remains block-local and
bounded by the 14 allocatable registers, so the extension retains
`O(instructions * 14)` time and `O(14)` additional space and does not create a
global stack-slot table or extend any live range.

The Step 69 candidate trace is
`target/heliodor/analysis/step112-state-load-cse-final`; the new complete trace
is `target/heliodor/analysis/step114-stack-home-load-cse`.  All three SIR dumps
remain byte-identical, with hashes `51b1befa...`, `7c19aec1...`, and
`fe40b3d4...`.  Complete MIR falls from 51,848,787 to 51,822,976 bytes and from
1,874,850 to 1,874,559 lines.  Spill frames remain 5,656 and 5,704 bytes.  The
five emitted bodies shrink by another 5,429 bytes: `eval_comb` by 2,695 bytes,
the fused body by 2,641 bytes, `eval_only` by 91 bytes, and `eval_apply_ff` by
2 bytes.

In the inspected `[sp + 40]` sequence, the two common-block memory operands
become one load followed by register arithmetic.  A path-local predecessor
load correctly remains because availability is not speculated across the
join.  A separate `[sp + 72]` pair also correctly remains: the allocator gave
the first reload and an intervening arithmetic result the same physical
register, destroying the reusable value before the second reload.  That is
direct evidence that allocation/coalescing choice, rather than a broader late
load rewrite, is the next structural target.

Six focused availability regressions cover state and stack reuse, base and
range separation, overlapping writes, physical-register redefinition,
copy-fold ordering, and destination-register preference.  The library suite
passes 1,055/1,055, native testbench 60 passed with one ignored, counter 9
passed with three ignored, and package/workspace checks, package all-target
strict Clippy, formatting, diff, and clean Heliodor source checks pass.  The
trace-free non-LTO interval-allocator run compiled in 342.395 s, reached
`reboot: Power down` and exactly `cy=9ae070 x3=aa pass=1`, and executed in
96.556 s.  This single sample is 3.600 s below Step 69 but is not sufficient to
claim a stable throughput gain.

Status: **unchanged private stack homes are no longer reread while an exact
physical value survives; Linux semantics and bounded invalidation are
verified, and avoidable allocator-created register destruction remains open**.

### Step 71: Allocate block-local reload regions before one-use fallback

The complete Step 70 MIR showed that the remaining `[sp + 72]` pair was not
primarily a late-load-availability problem.  Before allocation, one value
`v50537` fed a nearby add and subtract in the same block.  The terminal spiller
nevertheless created a distinct reload VReg for every use:

```text
v187911 = load.i64 [sp + 72]
v50666 = add.w32 v219, v187911
...
v187912 = load.i64 [sp + 72]
v50667 = sub.w32 v219, v187912
```

The resulting x86 read the same home twice with `add r9d,[rsp+48h]` and
`sub r11d,[rsp+48h]`.  No color choice could share those reloads because the
spiller had already represented them as unrelated one-use SSA definitions.

Terminal spilling now partitions semantic instruction uses by basic block and
orders each group by its stable instruction slot.  The first use dominates the
later uses in that block, so a multi-use group first becomes one ordinary
`RegisterRegion` whose reload is inserted at the first use.  That region goes
back through the same greedy queue, constraints, interference matrix, and
coloring as every other machine interval; the spiller does not invent or pin a
physical register.  Phi-edge uses remain singleton materializations.  If the
spilled source is already one register region whose complete use set is that
single block-local group, its uses become singleton materializations instead
of recreating the same topology.  This permits at most one topology-reducing
local retry.  Partitioning costs `O(U log U)` time in the worst case and `O(U)`
additional memory for a terminal spill with `U` uses; it does not duplicate the
CFG or construct an all-pairs interference graph.

The actual-scale run also exposed a terminal def-to-root-home-store range whose
semantic use set was empty.  Re-spilling that point-to-store transition cannot
shorten it.  The spiller now retires only its stale semantic-region ownership,
after which the unchanged exact machine interval re-enters the allocator as a
`Fixed` range.  A dead definition with no machine use is classified the same
way.  Both cases still require a physical destination for their defining
instruction and neither removes an observable store.

The complete candidate trace is
`target/heliodor/analysis/step115-block-local-reload-regions`.  All three SIR
dumps are byte-identical to Step 70, with hashes `51b1befa...`, `7c19aec1...`,
and `fe40b3d4...`.  In the inspected block, post-allocation MIR now contains
one reload:

```text
v184543 = load.i64 [sp + 72]
v50666 = add.w32 v219, v184543
...
v50667 = sub.w32 v219, v184543
```

The emitted sequence correspondingly contains one `mov rax,[rsp+48h]` followed
by `add r9d,eax` and `sub r11d,eax`.  Complete MIR falls from 51,822,976 to
51,230,860 bytes and from 1,874,559 to 1,853,193 lines.  The five emitted bodies
shrink by 33,407 bytes in total: `eval_comb` by 15,301 bytes, the fused body by
17,072 bytes, `eval_only_ff` by 1,022 bytes, and `eval_apply_ff` by 12 bytes;
`apply_ff` is unchanged.  This is not a uniform spill-frame improvement:
`eval_comb` grows from 5,656 to 5,672 bytes and `eval_only_ff` from 224 to 232
bytes, while the other three frames remain 0, 88, and 5,704 bytes.

Focused regalloc tests pass 313/313, including one-block grouping, bounded
fallback, store-only ownership retirement, and dead-result classification.
The library suite passes 1,056/1,056, native testbench 60 passed with one
ignored, counter 9 passed with three ignored, and workspace all-target check,
package all-target strict Clippy, formatting, and diff checks pass.  The
complete trace took 354.506 s.  The trace-free non-LTO interval-allocator run
compiled in 355.529 s, reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`, and executed in 93.636 s.  Relative to Step 70's
single adjacent run, execution is 2.920 s faster while compilation is 13.134 s
slower; one execution sample is not a stable throughput claim.

Status: **terminal spilling exposes a conventional block-local reload live
range to ordinary allocation before bounded one-use fallback; exact Linux
semantics and a substantial code-size reduction are verified, while compile
latency, spill-frame growth, global phi coalescing, and complete region-cost
selection remain open**.

### Step 72: Transactional final coloring of loop-phi components

The complete Step 71 assignment exposed two loop-carried logical values whose
colors crossed twice in one no-change iteration.  The header values `v767` and
`v768` occupied `r9` and `r8`, while the inner join values `v783` and `v784`
occupied `r8` and `r9`.  SSA destruction therefore emitted `xchg r9,r8` on the
no-change arm and `xchg r8,r9` again on the loop backedge.  The two exchanges
cancel dynamically, but neither pairwise recolor can run while the other
logical value still occupies both candidate colors.

An initial trial published each CSSA backedge source/destination relation as an
ordinary allocator affinity.  It removed the local pair, but changed greedy
occupancy before spilling.  In an unrelated hot loop it created an additional
allocator phi and stack round trips, grew the `eval_comb` and fused frames to
5,688 and 5,712 bytes, and regressed the full run to 358.311 s compile and
96.313 s execute.  That trial is rejected and none of its early-affinity path
remains.

The retained implementation records the verified natural-loop relation
separately from normal copy/phi affinities.  Only after every split, spill,
reload, and ordinary greedy assignment is complete does it find the active
ordinary-affinity connected components named by those descriptors.  For each
loop header it:

1. rejects a component if any two member intervals interfere, any member is
   inactive, or the CSSA source/snapshot/destination path no longer exists;
2. removes all eligible components from the exact interval-union matrix at
   once;
3. computes each component's common allowed and externally free registers;
4. solves a maximum-retained-live-length, distinct-register matching for all
   components at that header; and
5. publishes the complete matching or restores every original matrix
   membership without changing the assignment vector.

With `K <= 16` target registers and `G <= K` eligible components at one header,
matching takes `O(G * K * 2^K)` time and `O(G * 2^K)` parent bytes.  Component
discovery is linear in reached ordinary-affinity edges and uses one temporary
four-byte mark per allocation VReg.  It neither duplicates CFG state nor builds
an all-pairs interference graph.  The regression fixture deliberately crosses
two components so either one alone has no common color; it proves both exact
rollback and successful simultaneous exchange.

The complete retained trace is
`target/heliodor/analysis/step117-final-loop-phi-bundles`.  Pre-optimized,
post-optimized, and native-optimized SIR remain byte-identical to Step 71 with
hashes `51b1befa...`, `7c19aec1...`, and `fe40b3d4...`.  More importantly, the
concatenated pre-register-allocation MIR sections are also byte-identical with
hash `cffb7a19...`; the spill/split plan has not moved.  The final assignment
puts `v767/v783` and their snapshots in `r8`, and `v768/v784` and their
snapshots in `rsi`.  The no-change arm and loop backedge no longer exchange
them; one exchange remains only on the state-changing arm.

Post-allocation copy and reload cleanup then reduces complete MIR from
51,230,860 to 49,446,692 bytes and from 1,853,193 to 1,791,280 lines.  Spill
frames change from `5672/0/88/232/5704` to `4344/0/88/216/4368` bytes for the
five native bodies.  Their emitted endpoints shrink by 133,623 bytes in total:
`eval_comb` by 46,459, `apply_ff` by 367, `eval_apply_ff` by 6,712,
`eval_only_ff` by 19,778, and the fused body by 60,307 bytes.

Focused regalloc tests pass 315/315, including atomic crossed-color exchange
and failed-matching rollback.  The library suite passes 1,058/1,058, native
testbench 60 passed with one ignored, counter 9 passed with three ignored, and
workspace all-target check, package all-target strict Clippy, formatting, and
diff checks pass.  The complete trace took 98.843 s.  Two trace-free non-LTO
runs both reached `reboot: Power down` and exactly
`cy=9ae070 x3=aa pass=1`; compile intervals were 75.299 s and 74.672 s, while
generated-code execution took 84.421 s and 85.412 s.  Their 84.917 s execution
mean is 9.3% below the adjacent retained Step 71 sample of 93.636 s.

Status: **loop-carried copy/phi components are recolored atomically only after
spill placement; exact Linux semantics, pre-allocation identity, rollback, a
large frame/code reduction, and a repeated generated-code speedup are
verified**.

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
| 30d strict-SSA SplitEditor topology | this step | diamond and loop pruned-IDF/dominator-rename regressions 2/2; regalloc 256/256 | lib 876/876; strict clippy and format pass | production path is intentionally unchanged; no Linux or timing claim yet | n/a | legal copy cuts, loop phis, and exact child ranges are constructed; ownership/work-queue connection remains open |
| 30e machine-interval representative ownership | this step | empty-semantic-use split representative rebuild; regalloc 257/257 | lib 877/877; strict clippy and format pass | production path is intentionally unchanged; no Linux or timing claim yet | n/a | machine uses, not root-use subsets, own live ranges; generic machine spilling remains before production switch |
| 30f production strict-SSA splitting and machine spilling | this step | production split-to-machine-spill lowering regression; allocation split 14/14; regalloc 259/259 | lib 879/879; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; non-LTO format/check/clippy gates pass | pass through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1`; complete SIR/MIR byte-identical to Step 29e | trace 53.926 s; full code generation 52.491 s; simulation 116.325 s | every useful split product re-enters the queue and only `Spill` materializes it; `JointAllocationSession` removal remains |
| 30g conventional interval/matrix/base ownership | this step | greedy owner retention 1/1; allocation reallocate 12/12; allocation split 14/14; regalloc 259/259 | lib 879/879; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; non-LTO format/check/clippy/docs gates pass | release/LTO pass through `reboot: Power down` and exactly one `cy=9ae070 x3=aa pass=1`; complete SIR/MIR byte-identical to Step 30f | trace 56.072 s; release code generation 51.122 s; simulation 131.346 s | `JointAllocationSession` and production legacy split context removed; machine intervals, matrix, base queue, and spiller have separate owners |
| 31 no-layer effect stream and sparse MIR memory dependencies | this step | parser scheduler 18/18; memory effects 6/6; MIR scheduler 16/16 | lib 891/891; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; non-LTO format/check gates pass | normal testbench passes through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1`; complete SIR/MIR retained | trace 57.512 s; full code generation 57.777 s; simulation 133.430 s | layer/frontier batches removed; path-width pressure and unconditional block-local constants rejected; no throughput gain claimed; range StateSSA/lazy writeback remains next |
| 32a--32b sparse terminal StateSSA and lazy residency graph | this step | range StateSSA 8/8; residency/writeback 6/6 | lib 905/905; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; non-LTO check/strict clippy/format/diff gates pass | production remains disconnected; no Linux semantic or timing claim | n/a | terminal visibility now creates exact phis; phi SCC inheritance and allocator-selected branch/shared writeback clusters are represented without eagerly changing SIR |
| 32c allocation-owned packed-state operations and sparse physical MemorySSA | this step | state homes 8/8; allocation IR 21/21 | lib 913/913; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; package all-target strict clippy and non-LTO check/format/diff gates pass | production range lowering remains disconnected; no Linux semantic or timing claim | n/a | full machine-word stores/reloads enter exact allocation liveness; independent sparse MemorySSA rejects every wrong-path or overlapping reload before atomic MIR publication |
| 32d production state-home proof and eager-promotion rejection | this step | regalloc 278/278; final-write identity accepts 40 disjoint inserted writes and rejects a reaching overlap | lib 924/924; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; non-LTO check/format gates pass | pass through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1` | trace 89.995 s; full compile 86.870 s; execute 128.011 s | final MIR proof is sound; eager whole-version promotion raises the main frames and regresses Step 30f execution, so use-cluster allocation replaces it next |
| 33 source-MemorySSA SLT lowering regions | this step | shared analyses 39/39; parser scheduler 16/16; observer/cascade/false-loop 163 passed | lib 920/920; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check/strict clippy, format, docs, and diff gates pass | non-LTO and release/LTO both pass through `reboot: Power down` with exactly one `cy=9ae070 x3=aa pass=1` | non-LTO compile 163.267 s / execute 132.472 s; release compile 152.135 s / execute 133.980 s | bit-range Def/Use and SCC fences replace effect-only ready ordering; semantic checkpoint complete, no throughput gain claimed; allocator-selected use-cluster promotion remains open |
| 34 exact aggregate projection and same-range forwarding | this step | range lowering 3/3; forwarding 5/5; shared analyses 39/39; parser scheduler 16/16 | lib 926/926; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, docs, and diff gates pass | optimized non-LTO and final release/LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | range-only non-LTO compile 160.558 s / execute 112.922 s; final non-LTO 156.443 s / 110.254 s; release 149.973 s / 110.762 s | static field Loads retain exact ranges and cached snapshots; two-state unsigned same-width Store values forward directly; final non-LTO execute sample is 16.8% below Step 33 |
| 35 overlapping narrow-load coalescing | `103c9985` | covering-wide-load regression | retained Step 34 common gates | non-LTO pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | compile 155.724 s; execute 108.799 s | a required complete-object load no longer disables sharing equal narrow word projections; scheduler unchanged |
| 36 sparse object MemorySSA write state | `84fd5861` | sparse CFG/SSA 5/5; executable ISel state/metadata 2/2 | lib 934/934; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored | non-LTO pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | compile 143.852 s; execute 108.252 s | first and dominating-active object states remove redundant lowering; gain is small and disjoint chunk first-write proof remains open |
| 37 range-aware sparse chunk MemorySSA | `85952b9d` | sparse CFG/range SSA 8/8; executable chunk/data/metadata 1/1 | lib 938/938; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check and strict clippy pass | non-LTO pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | trace 118.702 s; full compile 117.173 s; execute 107.836 s | full MIR -13.8% and compile -18.5% versus Step 36; execute -0.4%, so repeated dirty/summary word updates remain open |
| 38 hierarchical sparse metadata state | this step | dirty-word assertions in range SSA 8/8; executable one-summary-update regression | lib 938/938; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check and strict clippy pass | two non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | trace 113.106 s; compile 111.882 / 111.514 s; execute 109.449 / 105.793 s | summary updates collapse to one per proved dirty word; MIR -6.4%; runtime effect varies and dirty-word update coalescing remains open |
| 39 explicit indexed metadata RMW | this step | MIR operands/emission; exact read+write effects; scheduler barrier | native backend 442/442 in the final combined source | non-LTO pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | compile 110.487 s; execute 110.961 s | x86 memory-destination OR removes the loaded metadata VReg; MIR 141,698,513 bytes; no stable runtime claim |
| 40 straight-line dirty-word batching | this step | sparse batch close/run 3/3; executable multi-run bitmap regression | native backend 442/442 in the final combined source | non-LTO pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | compile 108.349 s; execute 112.165 s | one proved same-word run emits one metadata mask; MIR 133,989,572 bytes; scheduler and data-Store order unchanged |
| 41 bounded native element-layout preservation | this step | load/store coalescing boundary regressions | final combined lib 947/947; native backend 442/442 | non-LTO pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | compile 105.731 s; execute 108.420 s | native layout intent prevents SIR coalescing from repacking small padded arrays; packed targets unchanged; MIR 133,726,550 bytes |
| 42 direct whole-element indexed access | this step | executable 12-bit indexed load/store and padding canonicalization | lib 947/947; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check and strict clippy pass | final non-LTO pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete trace byte-identical after target scoping | trace 110.122 s; full compile 107.693 s; execute 105.529 s | direct indexed scalar access; MIR 133,426,760 bytes; historical timing variance prevents a large speed claim; scheduler unchanged |
| 43 commit-independent sparse whole-zero fill | this step | demand-driven exact-zero, coverage/visibility, `MemFill` effects/emission, eval-only integration, scheduler overlap regressions | lib 957/957; native backend 450/450; all-target check; strict clippy; format and diff checks | final-source release/LTO pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | intermediate non-LTO/release compile-only 64.899 / 62.750 s; final release compile 62.047 s / execute 111.351 s | Step 42 compile 107.693→about 62--65 s; 208,896-bit zero construction never enters MIR; no runtime-speed claim |
| 44 machine-width known-bits mask elimination | this step | MIR mask/constant/32-bit zero-extension 61/61 | lib 962/962; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and diff checks | final non-LTO and two release/LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete final SIR/MIR retained | final non-LTO compile 68.337 s / execute 107.693 s; final release compile 61.215 / 62.130 s, execute 109.005 / 107.524 s | redundant register/immediate 32/64-bit mask chains removed without scheduler changes; final release execute -2.1% / -3.4% versus Step 43 |
| 45 allocator-visible same-block value sharing | this step | MIR GVN/rematerialization/MemorySSA 64/64 | lib 965/965; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check, strict clippy, format, and diff checks | all candidate and fixed-CPU A/B/A/B runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete final SIR/MIR retained | fixed-CPU Step 44 execute 101.001 / 101.731 s; candidate 98.888 / 104.028 s | hot repeated byte/bit indices share one value; final code -22,895 bytes; execute mean unchanged, so no speed claim |
| 46 control-dependent masked array search | this step | masked-array search and CLI 7/7 | lib 972/972; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check, strict clippy, format, and diff checks | fixed-CPU parent/candidate/parent/candidate and final-source run all pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete final SIR/MIR retained | Step 45 execute 106.589 / 102.699 s; candidate 100.532 / 98.778 s; final source 99.781 s | 32 eager element loads and 96 comparisons become a set-bit search; scheduler unchanged; execute mean -4.8% |
| 47 CFG circular-priority recovery | this step | circular-priority and CLI 10/10 | lib 982/982; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check, strict clippy, format, and diff checks | fixed-CPU parent/candidate/parent/candidate and final release/LTO all pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete final SIR/MIR retained | parent execute 98.925 / 106.973 s; candidate 102.461 / 101.181 s; final release 101.637 s | 32 scalar priority iterations become packed rotate and CTZ; scheduler unchanged; mean -1.1% but paired directions disagree, so runtime effect is unconfirmed |
| 48 effect-DAG indexed writes and sparse marks | this step | scheduler 19/19; sparse fallthrough emission regression | lib 987/987; native execution 16/16; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored | non-LTO pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete SIR/MIR retained | compile 66.524 s; execute 103.476 s | artificial barriers removed; standalone `apply_ff` frame 232→216 bytes; fused frame and execution unchanged, so no speed claim |
| 49 direct hazard-free sparse state | this step | round-trip 4/4; publication hazards 7/7; direct bulk-zero plan/emission | lib 996/996; dynamic NBA 33 passed, 1 ignored; native execution 16/16; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored | two final-code non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete final SIR/MIR retained | compile 73.027 / 72.968 s; execute 93.902 / 92.780 s | sparse first-write copy, metadata, and tail Commit removed where complete-event CFG proves no observation; MIR -12.1%; mean execute -9.8%; compile regression remains open |
| 50 register-free sparse active bitmap | this step | mark/worklist emission; multiword and padding; exact effects/GVN/reload/scheduler dependencies | lib 997/997; dynamic NBA 33 passed, 1 ignored; cross-block NBA 11 passed, 1 ignored; FF 200 passed, 42 ignored; native execution 16/16; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored | two trace-free non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete final SIR/MIR retained | compile 80.238 / 79.904 s; execute 91.148 / 95.338 s | each remaining sparse mark is one register-free bitmap `bts`; fused code -15,031 bytes; spill frame unchanged; execution mean unchanged, so no speed claim |
| 51 machine-width algebraic identities | this step | direct full-word merge; all word32 ALU constants and identities; zero-extension preservation | lib 1000/1000; dynamic NBA 33 passed, 1 ignored; cross-block NBA 11 passed, 1 ignored; FF 200 passed, 42 ignored; native execution 16/16; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored | two trace-free non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete final SIR/MIR retained | compile 80.586 / 80.420 s; execute 93.621 / 93.701 s | overwritten destination load and zero-mask merge collapse to a direct store; MIR -530,306 bytes; fused frame unchanged; execution mean unchanged |
| 52 exact reconstruction recipe prefixes | this step | exact prefix sharing; distinct final SSA ownership | regalloc 294/294; lib 1002/1002; dynamic NBA 33 passed, 1 ignored; cross-block NBA 11 passed, 1 ignored; FF 200 passed, 42 ignored; native execution 16/16; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check/strict clippy/format | two trace-free non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; all SIR stages byte-identical | compile 80.720 / 80.216 s; execute 92.850 / 93.337 s | duplicate state load/mask prefixes become one exact DAG; MIR -1,189 bytes; frames unchanged; execution mean difference too small for a speed claim |
| 53 demanded-prefix state forwarding | this step | same/cross-block forwarding; full-width use; partial overwrite; MemoryPhi join | regalloc 299/299; lib 1007/1007; dynamic NBA 33 passed, 1 ignored; cross-block NBA 11 passed, 1 ignored; FF 200 passed, 42 ignored; native execution 16/16; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check/strict clippy/format | two trace-free non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; all SIR stages byte-identical | compile 81.079 / 81.077 s; execute 96.678 / 93.437 s | MemorySSA-proved wider state loads disappear; MIR -33,721 bytes; eval/fused raw frames -8 bytes; no runtime-speed claim |
| 54 controlled-join arm sinking | this step | BranchifyMux 46/46 including multi-load sinking, write barrier, repeated-predicate edge | lib 1009/1009; dynamic NBA 33 passed, 1 ignored; cross-block NBA 11 passed, 1 ignored; FF 200 passed, 42 ignored; native execution 16/16; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check/strict clippy/format | non-LTO pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete trace retained | compile 80.256 s; execute 93.630 s | five match-result loads move to the selected predecessor; generic predicate short-circuit trial rejected; hot backedge layout and phi copies remain |
| 55 post-RA hot-backedge layout | this step | emitter 17/17 including chain placement and true-edge fall-through | lib 1011/1011; dynamic NBA 33 passed, 1 ignored; cross-block NBA 11 passed, 1 ignored; FF 200 passed, 42 ignored; native execution 16/16; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; workspace check, package strict clippy, format | two trace-free non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; all SIR stages byte-identical | compile 81.074 / 80.882 s; execute 92.371 / 91.962 s | hot continuation falls through to its adjacent copy block; eval body -350 bytes; phi stack/register copies remain |
| 56 selector-disjoint predicate control flow | `a723fb96` | BranchifyMux 50/50 including selector dispatch, overlap rejection, condition normalization, and store barrier | lib 1015/1015; retained native execution and RTL integration gates | two trace-free non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; complete trace retained | compile 80.732 / 80.566 s; execute 92.905 / 91.147 s | only the selected payload executes; false-edge five-`xchg` cycle and three backedge stores disappear; execution mean unchanged |
| 57 constant-work machine-word sign replication | this step | ISel 30/30 including executable two-state/four-state repeated-MSB concat | lib 1016/1016; dynamic NBA 33 passed, 1 ignored; cross-block NBA 11 passed, 1 ignored; FF 200 passed, 42 ignored; native execution 16/16; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; workspace check, package strict clippy, format, docs | two trace-free non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; all SIR stages byte-identical | compile 80.348 / 80.444 s; execute 92.280 / 89.330 s | repeated sign bits lower to `neg; shl; or`; MIR -255,946 bytes and eval code -3,946 bytes; execution mean -1.3% |
| 58 alias-aware late state DSE | this step | MIR optimization 70/70; state promotion 15/15 | lib 1020/1020; workspace check, package all-target strict clippy, format, docs, and diff checks pass | parent/candidate and two additional candidate runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; parent complete trace is byte-identical to retained HEAD | paired compile 73.919 / 74.250 s; execute 91.640 / 91.492 s | MemorySSA-exposed intermediate byte stores disappear; MIR -452,380 bytes; paired execution unchanged |
| 59 constant-work wide repeated-bit chunks | this step | native ISel 31/31 including executable two-state/four-state 128-bit concat | lib 1021/1021; workspace check, package all-target strict clippy, format, docs, and diff checks pass | trace-free non-LTO run passes through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; all SIR stages byte-identical | compile 73.399 s; execute 91.489 s | two 64-step sign-fill ladders become `shr; neg`; MIR -275,556 bytes; execution unchanged |
| 60 allocator-visible sparse-commit clobbers | this step | executable per-region/worklist clobber and fall-through labels | lib 1023/1023; workspace check, package all-target strict clippy, and format pass | trace-free non-LTO run passes through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; two complete traces and all SIR stages are byte-identical | compile 73.593 s; execute 92.635 s | hidden 7-register per-commit and 14-register worklist saves removed; no replacement spill frame; execution unchanged |
| 61 full-domain indexed FF stores | this step | indexed recovery/options 7/7; working round-trip 7/7; commit hazards 7/7 | lib 1033/1033; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and diff checks pass | two byte-identical generated-code non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; batch and CLI traces are byte-identical to the sequential candidate | compile 62.845 / 63.080 s; execute 93.248 / 92.955 s | 64-way and four-port selector ladders become direct indexed stores; accidental 512-byte round trips removed; SIR/MIR shrink, execution mean unchanged |
| 62 lazy selector arms and guarded value diamonds | this step | BranchifyMux 50/50; guarded-region sinking 22/22 | lib 1035/1035; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check, all-target strict clippy, format, and diff checks pass | two trace-free non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; independently generated final traces are byte-identical | compile 62.985 / 65.635 s; execute 90.748 / 96.790 s | selector payloads and four guarded word divide/remainder regions become control-dependent; MIR -9,907 bytes; execution mean unchanged |
| 63 loop-backedge phi affinity through CSSA snapshots | this step | color 7/7; regalloc 302/302 | lib 1037/1037; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; all-target check/strict clippy, format, and diff checks pass | two trace-free non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; SIR and pre-allocation MIR are byte-identical to Step 62 | compile 63.032 / 62.776 s; execute 89.473 / 90.408 s | exact CSSA snapshots expose only natural-loop backedge affinities; concrete seven-value backedge loses two repeated `xchg`; execute mean -4.1% |
| 64 convergent machine-interval spilling and sparse split queries | this step | regalloc 304/304 | lib 1039/1039; all-target check, strict clippy, format, and diff checks pass | opt-in interval allocator passes through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | compile-only 330.241 s; full compile 332.005 s; execute 103.855 s | impossible simultaneous phi reload pressure removed; fused allocation now finishes; generated execution remains 15.5% slower than the Step 63 mean |
| 65 productive free-prefix selection and transactional stack homes | this step | regalloc 307/307 | lib 1042/1042; all-target check, package strict clippy, format, and diff checks pass | opt-in interval allocator passes through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1` | compile-only 228.722 s; full compile 228.524 s; execute 101.035 s | split edits -45%; compile -31%; execute -2.7% vs Step 64; production gap remains 12.3% |
| 66 trivial-value closure and dead-phi DCE | this step | MIR optimization 73/73 | lib 1045/1045; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check and format pass | opt-in interval allocator passes through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; all SIR dumps byte-identical | trace 227.768 s; full compile 228.244 s; execute 102.749 s | full MIR -970,944 bytes; eval/fused frames -488/-464 bytes; one execution sample does not establish a speed gain |
| 67 adjacent allocator-home forwarding | this step | stack/state forwarding 2/2 | lib 1047/1047; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; check and format pass | opt-in interval allocator passes through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; all SIR dumps byte-identical | trace 227.839 s; full compile 226.362 s; execute 106.059 s | adjacent stack/state reloads removed; eval/fused code -753/-803 bytes; no speed gain claimed |
| 68 physical-width widened loads | this step | native/non-native widened-load ISel 2/2 | lib 1049/1049; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; workspace check, package strict Clippy, format, and diff checks pass | opt-in interval allocator passes through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; all SIR dumps byte-identical | trace 230.028 s; full compile 226.570 s; execute 102.671 s | redundant `load.i64; mov.w32` removed; frames -40/-40 bytes; allocator perturbation leaves aggregate emitted code +355 bytes, so no speed gain claimed |
| 69 physically available state-load reuse | this step | post-RA state-load reuse 5/5 | lib 1054/1054; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; workspace all-target check, package all-target strict Clippy, format, and diff checks pass | opt-in interval allocator passes through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; parent/candidate SIR dumps byte-identical | parent/candidate trace 349.666 / 364.103 s; full compile 346.444 s; execute 100.155 s | repeated CSSA recipe loads reuse exact surviving physical values; emitted code -29,960 bytes; frames unchanged; no speed gain claimed |
| 70 physically available private stack reloads | this step | post-RA direct-load availability 6/6 | lib 1055/1055; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; workspace all-target check, package all-target strict Clippy, format, diff, and clean source checks pass | opt-in interval allocator passes through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; Step 69/candidate SIR dumps byte-identical | full compile 342.395 s; execute 96.556 s | repeated surviving stack-home values reuse one physical load; emitted code -5,429 bytes; frames unchanged; one sample is not a speed claim |
| 71 block-local terminal reload regions | this step | regalloc 313/313 | lib 1056/1056; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; workspace all-target check, package all-target strict Clippy, format, and diff checks pass | opt-in interval allocator passes through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; Step 70/candidate SIR dumps byte-identical | trace 354.506 s; full compile 355.529 s; execute 93.636 s | one reload live range serves same-block uses before bounded fallback; emitted code -33,407 bytes; execute sample -3.0%, compile +3.8% |
| 72 final loop-phi component coloring | this step | regalloc 315/315 | lib 1058/1058; native testbench 60 passed, 1 ignored; counter 9 passed, 3 ignored; workspace all-target check, package all-target strict Clippy, format, and diff checks pass | two non-LTO runs pass through `reboot: Power down` with exactly `cy=9ae070 x3=aa pass=1`; Step 71/candidate SIR and pre-RA MIR are byte-identical | trace 98.843 s; compile 75.299 / 74.672 s; execute 84.421 / 85.412 s | crossed loop components recolor in one rollback-safe matching; frames 5672/0/88/232/5704→4344/0/88/216/4368; emitted code -133,623 bytes; execute mean -9.3% |

## Related design records

- [Simulator architecture](./architecture.md)
- [Native register allocation](./native-register-allocation.md)
- [Branch-aware mux lowering](./branch-aware-mux-lowering.md)
- [JIT roadmap](./jit-roadmap.md)
- [Heliodor macro benchmark](../benchmarks/heliodor.md)
