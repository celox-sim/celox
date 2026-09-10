# Affine scheduling experiment

This is an opt-in, dependency-free implementation of bounded polyhedral
scheduling, with SLT and SIR adapters and executable x86 experiments. It
synthesizes statement schedules from dependence constraints and generates tiled
SIR. Normal compiler pipelines do not invoke it yet.

This page records the scheduling and first frontend experiments. The subsequent
[numeric tuning experiment](affine-tuning.md) adds coordinate search and
sharing-preserving partial unrolling, including fresh comparisons with ordinary
optimized Veryl units.

The later [coverage experiment](affine-scope.md) adds affine input strides and
partial reads, measures a two-candidate policy, and audits existing designs.

The measured result is an improvement in generated kernel execution, at an
additional compilation cost. It is not a whole-simulator speedup claim.

## Relationship to the papers

[Verma et al., 2026](https://arxiv.org/html/2609.03114v1), sections 3.1–3.4,
uses Pluto to select the transformation structure and seed. Coordinate-wise
hill climbing then tunes numeric parameters within that structure. Its CPU
evaluation tunes tile sizes. The hand-selected tile sizes on this page preceded
the [coordinate search and statistical stopping rule](affine-tuning.md).

This experiment implements the prerequisite scheduling stage, following the
distance objective, independent schedule rows, permutable bands, and SCC
splitting described in [Bondhugula et al., CC 2008, sections 3.2–3.6](https://www.csa.iisc.ac.in/~uday/publications/uday-cc08.pdf).
The treatment of read reuse and scattering-domain tiling also follows
[Bondhugula et al., PLDI 2008, sections 3–5](https://www.csa.iisc.ac.in/~uday/publications/uday-pldi08.pdf).

This is a deliberately restricted variant, not a replacement for the full
Pluto implementation. No Pluto binary, ISL, external ILP solver, or new Cargo
dependency is used. The published Jacobi mapping is a regression oracle;
there has been no comparison against an installed Pluto executable.

## Implemented algorithm

Each statement has a finite integer domain, its original lexicographic time,
and affine memory accesses. Conflicting accesses generate RAW, WAR and WAW
relations, restricted to the original execution order. Read/read relations
participate in the locality objective without imposing execution order.

For each schedule row, the solver chooses integer coefficients and a constant
for every statement. For an ordering dependence from `(S, i)` to `(T, j)`:

```text
delta = theta_T(j) - theta_S(i)
0 <= delta <= w
```

Read reuse contributes `abs(delta) <= w`. The lexicographic objective minimizes
`w` first, then coefficients and constants. Nullspace constraints require each
unfinished statement to gain an independent iterator row. Dependencies remain
active throughout a permutable band; after closing it, only dependencies with
zero distance on its entire prefix remain. Scalar SCC phases separate residual
statement dependencies when needed.

Parameters are specialized before solving. Thus `w` is a bound for one concrete
domain, rather than the original algorithm's symbolic `u*p + w`. Exact rational
vertices impose the universal affine inequalities over bounded dependence
polyhedra, the primal counterpart of affine-Farkas elimination. A checked
integer solver uses interval propagation and lexicographic branch splitting.
This avoids floating-point tolerances and enumerating loop instances during
compilation. It conservatively retains all conflicting pairs rather than
performing last-writer dependence elimination.

An independent verifier checks strict lexicographic legality, injectivity within
each statement, and componentwise band legality. Scanning inverts the schedule,
inserts tile coordinates, and projects the scattering domains with
Fourier–Motzkin elimination. Integer ceil/floor bounds and membership guards
preserve tails and negative coordinates. The SIR emitter derives a shared
interior interval from all statement domains, emits guards only in boundary
pieces, and reconstructs source iterators in the loop preheader. These code
generation details were necessary to obtain the measured improvements.

The Jacobi fixture synthesizes the following mappings, including the skew
coefficient and relative shift:

```text
S0(t, i): (t, 2*t + i,     0)
S1(t, j): (t, 2*t + j + 1, 1)
permutable band: dimensions [0, 2)
```

## Integration boundaries

| Component | Implementation | Current input contract |
| --- | --- | --- |
| Shared scheduler and scan | `celox-analysis::polyhedral` | Specialized domains and affine access relations |
| SLT adapter | `celox-slt::affine::extract` | Explicit array statements retaining iteration/write provenance; ordinary SLT expression lowering |
| SIR loop adapter | `celox-sir::affine::extract` | Complete canonical counted-loop CFG; access relations derived from instructions |
| Unrolled SIR bridge | `celox-sir::affine::recover_independent_stores` | Complete straight-line unit, immutable input objects, distinct complete output cells |
| Output | `Kernel::lower` | Independently verified schedule and optional tile sizes; verified SIR |

The SLT API binds separate signed 64-bit iterator inputs. Existing `ForFold`
trees carry scalar state and are rejected; automatic recovery of per-element
array updates from those trees is not implemented. Consequently, merely
building Celox with these modules does not optimize ordinary Veryl programs.

The SIR extractor supports sequences and imperfect nests with constant bounds,
unit increments, one induction phi per loop, and straight-line memory bodies.
It rejects carried scalar state, indirect or potentially wrapping indices,
out-of-bounds accesses, partial cells, unknown object shapes, events, captures,
commits, and potentially trapping division/remainder. Data arithmetic and
four-state behavior are retained in the original statement bodies. Distinct
object identities must denote disjoint semantic storage; a later layout alias
decision must revalidate lifetimes against the new schedule.

Default search limits are 8 statements, 4 iterator dimensions per statement,
coefficients in `[0, 8]`, constants in `[0, 32]`, and 2,000,000 analysis work
units. This scheduler does not search negative coefficients. The scanner
requires an integral inverse basis and supports tiling one verified band at a
time. Checked arithmetic, coefficient bounds, and work budgets can reject an
otherwise legal optimization. Every caller must retain the original unit on
an extraction, scheduling, verification, or lowering error.

## Measurements, 2026-09-11

Host: AMD Ryzen 7 9800X3D, 96 MiB L3, Linux under a Microsoft hypervisor.
Rust 1.98.1, `heliodor-dev` profile. Both variants use the same existing SIR
cleanup, native MIR optimization, register allocation, and x86 emitter.
Execution is single-threaded, with the process pinned to CPU 2.

The baseline is a directly constructed canonical SIR CFG, independent of the
new scan generator. Both fixtures use 64-bit wrapping integer data in two-state
mode. Shift produces an intermediate array and then reads a three-element
neighborhood in a separate loop. Jacobi performs 16 three-point stencil steps,
each followed by a separate copy-back loop. The CSV `steps` column records the
command-line argument; only Jacobi uses it.

The table reports medians of 15 invocations. Baseline/candidate ordering
alternates; initialization and full semantic-state comparison occur outside
the timers. Each row remeasures its baseline. CSV files also contain quartiles,
and the text files contain all samples and compilation diagnostics.

| Kernel | Elements | Variant | Baseline ms | Candidate ms | Speedup |
| --- | ---: | --- | ---: | ---: | ---: |
| Shift | 262,144 | Synthesized schedule | 0.300 | 0.287 | 1.047x |
| Shift | 262,144 | Tile 256 | 0.319 | 0.356 | 0.896x |
| Shift | 8,388,608 | Synthesized schedule | 10.509 | 9.998 | 1.051x |
| Shift | 8,388,608 | Tile 4096 | 9.677 | 10.355 | 0.935x |
| Jacobi | 262,144 | Synthesized schedule | 4.279 | 3.781 | 1.132x |
| Jacobi | 262,144 | Tile 16x4096 | 4.385 | 4.033 | 1.087x |
| Jacobi | 8,388,608 | Synthesized schedule | 143.272 | 122.470 | 1.170x |
| Jacobi | 8,388,608 | Tile 16x4096 | 139.997 | 123.071 | 1.138x |

Full results: [medium CSV](../benchmarks/data/affine-2026-09-11/medium.csv),
[medium samples](../benchmarks/data/affine-2026-09-11/medium.txt),
[large CSV](../benchmarks/data/affine-2026-09-11/large.csv),
[large samples](../benchmarks/data/affine-2026-09-11/large.txt).

Jacobi scheduling took 31.5–32.1 ms, scan/SIR generation 1.7–2.6 ms, and native
compilation 3.7–7.3 ms depending on the candidate. The original native compile
took 1.1–1.5 ms. These compilation timings are individual measurements, not
medians. This is a compilation-time increase. Using the untiled medians gives
an illustrative amortization of roughly 74 invocations for the medium fixture
and 2 for the large one; actual HDL workloads still need measurement.

The most promising result here is synthesized skew/fusion with good code
generation. Additional tiling did not improve on the untiled schedule in the
recorded run. Shift's small gains have overlapping interquartile ranges and
are not strong evidence. These are two synthetic kernels on one host, without
a statistical significance claim or comparison to C compilers/Pluto.

The following experiment checks the frontend integration assumption with actual
Veryl source. Parameter tuning should follow a stable schedule/code-generation
baseline and should account for compilation cost. The separate
[numeric tuning follow-up](affine-tuning.md) now implements coordinate search.

## Veryl frontend experiment, 2026-09-11

The first commit (`ba134ab90`) measured constructed loop kernels. The follow-up
uses Veryl source for array mapping, a producer/consumer stencil, scalar
reduction, and an FF shift register. `affine_veryl audit` calls the ordinary
`compile_to_sir` entry point with all existing optimizations enabled and captures
both sides of SIR optimization. These are frontend-compiled microkernels, not
representative application or whole-simulator benchmarks.

At 32 elements, in both two- and four-state mode:

| Veryl case | Existing SIR loops | Stores before/after SIR optimization | Canonical extraction | Unrolled bridge before optimization |
| --- | ---: | ---: | --- | --- |
| Map | 0 | 32 / 8 | Rejected | 1 statement |
| Producer/consumer | 0 | 64 / 16 | Rejected | 4 statements, including boundary stores |
| Reduction | 1 | 1 / 1 | Rejected | Rejected |
| FF shift | 0 | 64 / 17 across 3 units | Rejected | Rejected |

The map and stencil are already expanded into scalar expression DAGs. The
stencil reads the original input directly, shares intermediate multiplications,
and has no remaining reads from its temporary array. Stores are grouped into
128-bit SIR operations. Native inspection finds **scalar arithmetic and grouped
scalar stores**, not SIMD arithmetic, in these fixtures. Merely counting wide
SIR stores would misidentify the cause of the baseline's performance.

The reduction retains a `ForFoldGroup` and a loop header with three parameters:
remaining count, source induction, and accumulated state. The first two are
loop control; accepting all additional parameters as independent induction
variables would erase a real recurrence. FF scheduling also requires its
region/commit and event semantics. Neither case enters the new bridge.

The bridge checks every store's typed expression DAG after translating static
input indices relative to its output index. It groups identical templates into
contiguous intervals, splits holes, and proves that each output cell is written
once and every loaded object is immutable. This permits reordering the store
families before the shared dependence/reuse scheduler. It is not a general
reconstruction of source loops, and does not recover mutable-array dependences
already eliminated by the frontend. Effects, partial cells, overlapping writes,
mutable inputs and unsupported control flow are rejected. Statement and work
limits bound the experiment, without adding dependencies.

Boundary-only statements initially caused the scanner's shared interior to be
empty. The emitter now partitions a statically bounded innermost coordinate at
all statement endpoints and emits only the active statements in each interval.
There are at most twice as many pieces as statements, independent of trip count.
Integer ceil/floor and singleton intervals are tested. Dynamic-prefix domains
retain the existing guarded scanner.

### Execution and code generation

Same host/profile/CPU pinning as above. The baseline uses the fully optimized
SIR produced by `compile_to_sir`, with its packed layout. The candidate is
recovered from pre-optimization SIR and then scheduled. Both pass through the
existing merged-unit SIR optimizer and x86 backend. This comparison does not
include the native simulator's dispatch loop or claim equivalence to every
layout choice in `SimulatorBuilder::build_native`.

Each row is the median of 15 batches, alternating baseline/candidate order.
Each batch repeats an invocation `clamp(2,000,000 / n, 32, 100,000)` times;
initialization and stable-state comparison are outside the timer. Inputs change
between batches, not within a batch, so this measures hot, repeatedly evaluated
kernels. Four-state batches include pseudo-random unknown masks. Both instruction
streams are warmed first. Raw samples and quartiles are retained; no statistical
significance claim is made. Compilation timings are single observations.

| Case | Elements | Mode | Existing µs | Scheduled µs | Existing / scheduled |
| --- | ---: | --- | ---: | ---: | ---: |
| map | 32 | 2-state | 0.0237 | 0.0317 | 0.747x |
| map | 32 | 4-state | 0.0298 | 0.0512 | 0.583x |
| producer_consumer | 32 | 2-state | 0.0288 | 0.0470 | 0.613x |
| producer_consumer | 32 | 4-state | 0.0593 | 0.1357 | 0.437x |
| map | 512 | 2-state | 0.0807 | 0.2033 | 0.397x |
| map | 512 | 4-state | 0.3386 | 0.5496 | 0.616x |
| producer_consumer | 512 | 2-state | 0.1772 | 0.6575 | 0.270x |
| producer_consumer | 512 | 4-state | 1.0423 | 2.2808 | 0.457x |
| map | 4,096 | 2-state | 1.2232 | 1.5960 | 0.766x |
| map | 4,096 | 4-state | 2.6425 | 4.2566 | 0.621x |
| producer_consumer | 4,096 | 2-state | 1.8889 | 4.7510 | 0.398x |
| producer_consumer | 4,096 | 4-state | 7.9602 | 18.3433 | 0.434x |

All measured schedule/tile variants lose execution throughput to this baseline.
Tile 64 does not establish an improvement over the untiled schedule; on the
stencil it can also reintroduce substantial dynamic boundary overhead. The
recovered scalar loops discard cross-iteration expression sharing and add loop
control. Instruction-level parallelism and memory-access form may also
contribute; their individual costs have not been isolated.

The code-size and backend-time tradeoff is large at 4,096 elements:

| Case / mode | Existing bytes | Scheduled bytes | Existing backend ms | Scheduled backend ms | Recovery + schedule + lowering ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| map / 2-state | 83,357 | 78 | 212.386 | 0.669 | 4.570 |
| map / 4-state | 211,565 | 168 | 613.285 | 0.975 | 3.735 |
| producer_consumer / 2-state | 143,571 | 222 | 323.031 | 1.287 | 12.485 |
| producer_consumer / 4-state | 543,867 | 573 | 2086.054 | 1.820 | 12.877 |

The common Veryl-to-optimized-SIR phase still costs 450–1,439 ms for these
4,096-element inputs and is included separately as `compile_ms` in the CSV.
The experiment already pays for expansion and ordinary SIR optimization before
recovering a loop. The backend timings therefore demonstrate a potential
compilation-cost tradeoff, not a measured default-pipeline compilation speedup.
For long simulations, the added per-evaluation runtime can outweigh that saving.

This changes the adoption priority: preserve array iteration provenance early,
and preserve or deliberately trade off existing cross-iteration sharing and
unrolling when generating a schedule. A profitability/search layer must retain
the existing optimized unit as a candidate. The synthetic Jacobi result does
not justify enabling recovered scalar loops by default. This bridge and endpoint
partitioning are integration/code-generation experiments. The
[subsequent parameter search](affine-tuning.md) builds on them and addresses
sharing while keeping the existing optimized unit as the acceptance baseline.

Artifacts: [eligibility CSV](../benchmarks/data/affine-veryl-2026-09-11/audit-32.csv),
[rejection details](../benchmarks/data/affine-veryl-2026-09-11/audit-32.txt),
[32-element CSV](../benchmarks/data/affine-veryl-2026-09-11/bench-32.csv),
[512-element CSV](../benchmarks/data/affine-veryl-2026-09-11/bench-512.csv),
[4,096-element CSV](../benchmarks/data/affine-veryl-2026-09-11/bench-4096.csv),
and [native inspection](../benchmarks/data/affine-veryl-2026-09-11/native-inspection.txt).
Each timing CSV has a sibling `.txt` containing schedules and all samples.

## Reproduction and validation

```sh
cargo build --offline --profile heliodor-dev -p celox --example affine_scheduling
taskset -c 2 target/heliodor-dev/examples/affine_scheduling 262144 16 15
taskset -c 2 target/heliodor-dev/examples/affine_scheduling 8388608 16 15

cargo test --offline -p celox-analysis -p celox-sir -p celox-slt --lib
cargo test --offline -p celox --test affine_scheduling
cargo test --offline -p celox --no-default-features --test affine_scheduling
cargo clippy --offline -p celox-analysis -p celox-sir -p celox-slt --all-targets -- -D warnings
cargo clippy --offline -p celox --test affine_scheduling --example affine_scheduling -- -D warnings
cargo fmt --all -- --check
```

The focused tests check exact integer optimization against exhaustive search,
rational extrema, independent schedule legality, shifted producers/consumers,
the published Jacobi schedule, statement-specific axes, constrained unimodular
scans, negative coordinates, tails, disjoint interiors, and explicit rejection
of unsupported effects/index arithmetic. Differential execution covers SLT and
CFG extraction, original/scheduled/tiled SIR, two- and four-state data, the
interpreter, and x86 machine code. Two-state results also have an independent
Rust reference implementation. The interpreter-only integration suite runs
without `host-runtime`; the timing example requires x86-64 host execution.

Validation for this change passed 265 crate unit tests and 3 integration tests,
the 3 interpreter-only integration tests without default features, both focused
Clippy commands, and the workspace formatting check.

Follow-up reproduction (optional dump directories retain source/IR and native
images for inspection):

```sh
cargo build --offline --profile heliodor-dev -p celox --example affine_veryl
taskset -c 2 target/heliodor-dev/examples/affine_veryl audit 32 /tmp/affine-audit
taskset -c 2 target/heliodor-dev/examples/affine_veryl bench 32 15 /tmp/affine-native
taskset -c 2 target/heliodor-dev/examples/affine_veryl bench 512 15
taskset -c 2 target/heliodor-dev/examples/affine_veryl bench 4096 15
cargo test --offline --profile heliodor-dev -p celox-sir --lib
cargo test --offline --profile heliodor-dev -p celox --test affine_veryl --test affine_scheduling
cargo test --offline --profile heliodor-dev -p celox --no-default-features --test affine_veryl --test affine_scheduling
cargo clippy --offline -p celox-sir --all-targets -- -D warnings
cargo clippy --offline -p celox --test affine_veryl --test affine_scheduling --example affine_veryl -- -D warnings
cargo fmt --all -- --check
```

Follow-up validation passed 38 SIR unit tests, 6 integration tests, the same 6
integration tests without default features, focused Clippy, and formatting.
The actual frontend cases include a last-lane exception, holes/boundaries,
three element counts, wrapping 32-bit data, and known/X/Z values. Recovered,
scheduled and tiled units are checked against the original interpreter;
nineteen-element cases also compare all semantic cells in native execution.
Known data additionally has an independent Rust oracle. The endpoint regression
checks non-integral inequality bounds against independently rounded intervals.
