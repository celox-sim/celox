# Affine scheduling experiment

This is an opt-in, dependency-free implementation of bounded polyhedral
scheduling, with SLT and SIR adapters and executable x86 experiments. It
synthesizes statement schedules from dependence constraints and generates tiled
SIR. Normal compiler pipelines do not invoke it yet.

The measured result is an improvement in generated kernel execution, at an
additional compilation cost. It is not a whole-simulator speedup claim.

## Relationship to the papers

[Verma et al., 2026](https://arxiv.org/html/2609.03114v1), sections 3.1–3.4,
uses Pluto to select the transformation structure and seed. Coordinate-wise
hill climbing then tunes numeric parameters within that structure. Its CPU
evaluation tunes tile sizes. A few hand-selected tile sizes, as evaluated here,
are not an implementation of that search or its statistical stopping rule.

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
| SIR adapter | `celox-sir::affine::extract` | Complete canonical counted-loop CFG; access relations derived from instructions |
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

The next adoption step is preserving array-update provenance in actual frontend
SLT and measuring eligible HDL regions. Parameter tuning should follow a stable
schedule/code-generation baseline and should account for compilation cost.
The 2026 paper's coordinate search remains a separate, unimplemented stage.

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
