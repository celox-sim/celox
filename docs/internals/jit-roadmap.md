# JIT Roadmap

This note tracks the JIT work that should matter before adding a Veryl
verification-component ABI. The short version: Celox should not rely on a
background C/C++ compiler for its normal fast path. The project already has the
right foundation in the native x86-64 backend; the next step is to make it a
tiered, domain-aware JIT that can beat Verilator on the hot benchmarks without
destroying testbench/component scheduling performance.

## Baseline

Celox currently has three execution backends:

- `NativeBackend`: self-hosted x86-64 codegen through SIR -> ISel -> MIR ->
  MIR optimization -> register allocation -> x86-64 emission. This is the
  default on x86-64.
- `JitBackend`: Cranelift-based fallback and comparison backend.
- `WasmBackend`: Wasm codegen plus Wasmtime/browser execution.

The native backend is the strategic path. Cranelift is valuable as a fallback
and differential backend, but it should not be the ceiling for x86-64
performance. Running GCC/Clang behind the scenes would improve some AOT numbers,
but it makes compile latency, cache invalidation, diagnostics, and deployment
environment part of the runtime contract. That is the wrong default for an
interactive simulator.

Some current benchmark cases can still trail Verilator substantially. Treat
that as a JIT design problem, not as evidence that Celox should move to a C++
toolchain.

## Current Status

The first native-JIT improvement targets `linear_sec` bit placement and scalar
testbench-style I/O:

- MIR has BMI2 `pdep` in addition to the existing `pext` and `popcnt`.
- The optimizer folds chunked bit scatter/gather OR chains into `pdep`/`pext`.
  These folds are enabled only when the host CPU reports BMI2 support.
- Dynamic bit-toggle insertions are folded to `xor` when the source shape is
  provably a two-state single-bit toggle.
- Native scalar `set`/`get_as` uses direct unaligned scalar access for matching
  widths.
- `eval_comb_checked` bypasses observer/runtime-event bookkeeping when a
  program has no such sites.

On the local x86-64 benchmark slice, the generated DSE `linear_sec` comb kernel
measured about 6.9 ns/eval, while Verilator's matching `linear_sec` harness
measured about 14.6 ns/eval. The scalar `set`/`get_as` benchmark measured about
8.9 ms per million iterations, so the generated code itself is past the 2x
target and the public scalar API path remains about 1.6x faster. Default non-DSE
simulation still preserves more internal state than the Verilator harness and
remains a separate target.

### Heliodor Linux Boot Findings

Heliodor is now the macro benchmark for comparing Celox against the Veryl
native simulator. Veryl's public simulator benchmark describes a tiered design:
start quickly with a Cranelift backend, then switch to a GCC-optimized backend
when the compiled binary becomes available:
<https://veryl-lang.org/blog/veryl-simulator-performance/>.

Local Celox measurements on `test_soc_linux_boot` show a different bottleneck
from the small `linear_sec` kernels:

- Veryl `cc` baseline for the pinned Heliodor checkout completes the single
  Linux boot in roughly 66--70 s.
- Celox has never completed this full gate quickly. Earlier successful Celox
  records were compile-only, not Linux-boot completions. Before control-aware
  lowering, a representative partial run measured about 48 us per
  `eval_comb`. The current verified binary-CFG lowering measures about 36.3 us
  in a same-build 60-second window, but still reaches only about 700k of the
  observed 10.15M ticks. It therefore remains several times too slow and is
  not an accepted end-to-end result.
- Celox Cranelift is not a viable replacement for the custom native backend on
  this workload; its JIT/backend phase was substantially slower than native.
- The hot Celox native unit is `eval_comb`, not `eval_apply`. With the current
  binary-CFG lowering, a representative allocated `eval_comb` has about 202k
  MIR instructions, including about 21.5k stack loads and 10.5k stack stores.
- The largest `eval_comb` blocks are dominated by long mux chains. Some are
  direct case/decode chains, but the hottest 172-arm chains in blocks 432 and
  0 are accumulator-guarded priority encoders of the form
  `acc = mux(guard && acc == default, value, acc)`. In block 432, the SIR dump
  after the current stable optimizations still shows about 15k `Mux`, 8k
  `LogicAnd`, 8k equality checks, and 4k `Concat` operations.

Several instruction-count wins did not survive the Heliodor correctness/perf
gate:

- General narrow mux lowering to `else ^ ((then ^ else) & mask)` reduced MIR
  size but produced an x86 divide exception during the Linux boot.
- Replacing trigger `Select(cond, 1 << bit, 0)` with shift/setcc-style
  specialized code also produced an x86 divide exception in Heliodor.
- Making the existing div/rem emitter fully conservative by spilling the
  divisor through memory, or by declaring RCX as a div/rem clobber, avoided the
  crash but prevented the run from reaching the first 50k-tick timing marker
  within a 45 s timeout.
- Rewriting zero-extension `Concat([0..., low])` to an identity reduced local
  SIR work but increased Heliodor `avg_comb_us` to roughly 68 us.
- Replacing general narrow mux bit-blends with MIR `Select` passed small native
  tests but produced a divide exception during Heliodor.
- Adding a final `ReschedulePass` to `eval_comb` passed tests but slowed the
  Heliodor timed run to roughly 66 us per combinational evaluation.
- Rewriting accumulator-guarded priority encoders back to plain guard-only mux
  chains reduced some equality/logic work, but the Heliodor run failed to reach
  the first 300k-tick timing marker within a 70 s timeout. The long branchless
  mux dependency chain itself remains the problem.

The one accepted Heliodor-facing SIR change so far is conservative:

- `vectorize_concat` now leaves all `Concat` operations intact in 4-state mode,
  because bitwise/arithmetic rewrites normalize Z to X while `Concat` must
  preserve value and mask bits exactly.
- In 2-state mode, proven sign-extension concats such as
  `{low[MSB], ..., low[MSB], low}` are folded to a shift-left/arithmetic-shift
  pair. This reduced MIR counts but only improved the timed Heliodor run
  slightly.

Those rejected experiments exposed a coupled codegen/regalloc correctness bug,
not evidence that the peepholes themselves were sufficient. The replacement
allocator now models implicit RAX/RDX constraints and live-through clobbers,
and the emitter uses a proved stack copy when a divisor aliases RAX/RDX rather
than an untracked RCX scratch. Per-pass MIR verification and dedicated division
tests cover that boundary. With the scratch contract fixed, the measured
bottleneck is the remaining decision-region shape and its resulting pressure,
not a reason to revive the unsafe fast path.

### Branchy Case Lowering

Heliodor's decoder-heavy hot block is not primarily a missing peephole. The SIR
shape computes every case arm first, then selects with a long nested mux chain:

```text
cond_i = opcode ==? imm_i
arm_i  = expensive expression for case i
...
result = mux(cond_0, arm_0, mux(cond_1, arm_1, ... default))
```

That is a faithful hardware graph, but it is a poor software simulation shape
for large decoders. It extends every arm's live range across the full select
chain, inflates regalloc pressure, and executes arms that the current opcode
cannot observe. A plain reschedule pass cannot fix this because it must still
emit a linear program where all operands of each mux are already available.

The first implemented stage is cost-directed control preservation:

```text
entry:
  if cond_0 goto arm_0 else test_1
arm_0:
  compute arm_0
  jump join(arm_0_value)
test_1:
  if cond_1 goto arm_1 else default
...
join(result):
  use result
```

The primary transform happens while lowering the SLT expression DAG, before
both arms have been eagerly materialized. Shared descendants are computed once
in the dominator and only exclusive arm work enters the diamond. A later
`BranchifyMux` SIR pass can recover opportunities exposed by simplification,
but it is a cleanup complement rather than the primary decoder lowering.

Both stages make a strict local expected-cost decision. Expected skipped arm
work and the removed select must exceed branch control, static expected
misprediction, merge copies, and (for the SIR cleanup) values kept live through
the new diamond. Equality-to-constant decoder conditions receive a static
not-taken bias; other conditions use an even prior. There is no arm-count,
iteration-count, function-size, or CFG-size cap. Small cheap muxes remain
branchless because their local cost is unfavorable, not because a global
budget was exhausted.

Four-state muxes stay as dataflow muxes because their X/Z condition semantics
cannot currently be represented by SIR `Branch`. Pure/shared-definition and
memory-alias legality rules remain mandatory. The full relationship and cost
equation are documented in
[Branch-aware mux lowering](./branch-aware-mux-lowering.md).

The binary stage is not the completed Heliodor solution. Current lowering sees
22,344 muxes: 2,579 become verified CFG diamonds, 16,509 remain branchless
because their local expected cost is unfavorable, and 3,227 pass the cost test
but are conservatively retained because they contain deeper shared DAG nodes.
Only one shared node is currently hoisted. Those 3,227 cases require
dominance-aware expression placement: each node must be placed once in the
nearest common control region of its uses, rather than cloning the global
cache per arm or eagerly hoisting all shared work.

There is also a distinct multiway problem. Veryl's AOT path still invokes all
31 combinational chunks every tick, so its advantage is not event-driven
evaluation. GCC recognizes the generated equality/priority chains as decision
regions and emits 159 indirect jump-table dispatches, along with balanced and
partially if-converted tails. Celox must likewise recover a verified
same-selector decision region and choose among a value table, dense jump table,
sparse comparison tree, ordered wildcard chain, and small branchless tail by
target cost. This is a region transform above binary mux lowering, not another
`BranchifyMux` threshold.

The complete fixed phase order, provenance model, placement algorithm,
Decision verifier, target strategy selection, pressure cuts, and final
phi-congruence affinity are specified in
[Decision-region architecture](./decision-region-architecture.md). New code is
added verifier-first in that order; a failed plan cannot select the old
allocator or an unverified binary fallback.

A same-build A/B of the cleanup pass confirms the distinction. Explicitly
enabling `BranchifyMux` applied 464 additional diamonds, increased the spill
frame from about 79.2 to 84.3 KiB and effective edge copies from 10,110 to
14,206, while leaving `avg_comb_us` at roughly 36.1--36.4. It therefore remains
disabled in the O1/O2 presets.

## Goals

1. Make `NativeBackend` the clear x86-64 performance backend for both RTL-only
   and testbench-driven workloads.
2. Preserve low compile latency. A fast simulator that shells out to a C++
   optimizer for normal runs is not fast in the workflow that matters.
3. Keep component ABI support explicit at effect boundaries so component calls
   do not force full-settle behavior in tight loops.
4. Keep Cranelift and Wasm as correctness/comparison/fallback paths.

## Measurements First

Before changing codegen, add a benchmark matrix that records these dimensions
for every workload:

- backend: native, Cranelift, Wasm where applicable
- mode: build, raw `eval_comb`, raw `tick`, testbench VM, TypeScript/NAPI
- baseline: Verilator with the existing Google Benchmark harness
- circuit shape: scalar comb, wide comb, repeated generate lanes, stateful FF,
  memory-heavy FIFO, native testbench loop

The important diagnostic split is whether Verilator wins inside the generated
RTL function or because Celox pays scheduler/testbench/API overhead around it.
The existing `simulation.rs` benchmarks already have useful isolation cases;
extend that idea systematically rather than relying only on public headline
graphs.

## Native JIT V2

### 1. Domain Kernels

Today the simulator calls separate compiled functions for `eval_comb` and
per-event FF functions. That is a good general interface, but it leaves
performance on the table for common single-clock loops.

Add optional per-domain kernels for hot events:

```text
domain_tick(mem):
  eval_comb_if_needed
  eval_only_ff_for_domain
  apply_ff_for_domain
  eval_comb_after_commit
```

The generic `SimBackend` API can remain unchanged. `Simulator::tick` can use a
domain-kernel fast path when the backend exposes one and fall back to the
existing split calls otherwise.

This removes Rust dispatch between phases and gives the native backend a larger
optimization window. It is also the right insertion point for component support:
component staging/firing can disable or decorate the fast kernel only when a
component actually listens to that event.

### 2. Generated Testbench Kernels

The current compiled testbench is a Rust-side bytecode/statement executor. That
is much better than reparsing or interpreting Veryl IR directly, but tight
million-iteration testbench loops still bounce through Rust control flow.

Add a native testbench JIT tier for the common subset:

- counted `for` loops with static or narrow dynamic bounds
- `ClockNext`
- scalar/wide loads and stores
- assertions with compiled predicates
- return-free helper functions after inlining

The initial target is not full Veryl testbench coverage. It is the hot shape
already present in benchmarks:

```text
for i in 0..N {
  clk.next();
  sink = signal;
}
```

This tier should call compiled domain kernels directly. If a statement contains
an unsupported operation, keep using the existing executor for that region.

### 3. Effectful Component Calls

Component methods are effectful. Do not put them into ordinary expression
bytecode as if they were pure functions.

Lower component method calls into explicit testbench statements or hoisted
temporaries:

```text
tmp = component.method(args...)
x = tmp + y
```

At that boundary:

- settle only if the method arguments read dirty combinational state
- marshal arguments from the simulation buffer
- call the component host ABI
- write the return value if present
- mark dirty only if the method writes simulation-visible state

This avoids the naive rule "settle before every method call" becoming the
dominant cost in component-heavy tests.

### 4. Memory and Register Promotion

Verilator benefits from C++ optimizer visibility over object fields and local
temporaries. Celox should recover the same wins in MIR:

- promote repeated loads from Stable/Working regions inside a domain kernel
- avoid memory round-trips at execution-unit boundaries when the value remains
  local to the kernel
- coalesce adjacent scalar loads/stores into narrower or vector operations when
  profitable
- keep generated-loop lanes in registers or vector registers instead of
  materializing every intermediate to memory

The native backend already has SIR and MIR-level forwarding. The next step is
cross-EU promotion inside a fused domain kernel, where the optimizer can see
the complete hot path.

### 5. Lane and SIMD Codegen

Some Verilator comparisons are repeated-lane designs: many counters, encoders,
decoders, or generate-expanded instances with the same operation over adjacent
state. Scalar x86 emission will not reliably beat optimized C++ there.

Add a lane detector before native ISel:

- identify repeated independent stores with identical operations and adjacent
  memory layout
- lower them into MIR vector operations where widths and alignment permit
- target at least AVX2 for x86-64, with scalar fallback

This is especially relevant for `top_n1000`-style counter benchmarks and
stdlib modules with regular arrays.

### 6. Instruction Selection Targets

Keep improving scalar instruction quality where it directly maps to RTL idioms:

- `popcnt` for count-one reductions
- `pext`/`pdep` for bit gather/scatter patterns when the CPU supports BMI2
- narrower 32-bit operations when high bits are provably dead
- immediate addressing and memory operands instead of load-then-op sequences
- branchless selects for small muxes
- specialized wide shifts and masks to avoid quadratic code expansion

CPU feature selection must be explicit in the compiled-code cache key.

## Relationship to Cranelift

Cranelift should stay:

- fallback for non-x86-64 targets
- differential backend for tests
- fast path for designs the native backend cannot yet lower

It should not be where x86-64 performance work primarily lands. The native MIR
is closer to RTL semantics and easier to tune for memory layout, lane
recognition, component boundaries, and simulator-specific calling conventions.

## Implementation Order

1. Add backend-tagged benchmark reporting for native vs Cranelift vs Verilator
   on the existing suite.
2. Add native domain-kernel hooks behind an optional backend capability, first
   for single-event `tick`.
3. Add a native testbench JIT for counted loops containing `ClockNext`,
   signal reads, signal writes, and simple assertions.
4. Implement effectful component-call lowering on top of the testbench
   statement layer, not inside pure expression bytecode.
5. Add cross-EU memory/register promotion inside fused domain kernels.
6. Add repeated-lane detection and SIMD lowering for regular generated arrays.
7. Expand CPU-feature-specific instruction selection and cache keys.

Each phase needs a semantic differential test across native, Cranelift, Wasm,
and the Veryl reference simulator where available. Performance work without
cross-backend correctness tests is too easy to get wrong in an HDL simulator.

## Non-Goals

- Do not replace the default path with GCC/Clang AOT.
- Do not make component support depend on full `eval_comb` before every host
  call.
- Do not remove Cranelift; it remains useful as a fallback and correctness
  reference.
- Do not optimize only benchmark harness overhead. Raw generated-code wins must
  be visible in isolation benchmarks as well as public end-to-end numbers.
