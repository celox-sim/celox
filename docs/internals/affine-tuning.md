# Numeric tuning of affine code generation

This extends the [affine scheduling experiment](affine-scheduling.md) with a
bounded numeric search and partial unrolling that preserves shared expressions.
It fixes the main weakness of the previous Veryl experiment: recovered scalar
loops were smaller and cheaper to compile, but discarded sharing and lost
execution throughput. The new experiment finds faster code for some
frontend-compiled microkernels while retaining the existing optimized unit as
the acceptance baseline. No compiler default or dependency changes.

The subsequent [coverage and two-candidate experiment](affine-scope.md) widens
input recovery, measures cheaper selection on additional families, and audits
the remaining boundaries in existing repository designs.

The most repeatable result is the four-state producer/consumer with all input
values known: three independent searches at each size all passed a fresh
comparison against the existing implementation. Throughput ratios were
1.048–1.059 at 32 elements, 1.079–1.132 at 512, and 1.106–1.253 at 4,096.
Several other conditions fail that comparison, and the search costs seconds
on the stencil. This supports further work on selective optimization, not
unconditional replacement of the existing code.

## Relationship to the paper

[Verma et al., sections 3.2–3.4](https://arxiv.org/html/2609.03114v1) tune numeric
parameters after choosing a transformation structure. The implementation now
includes pooled best-fit coordinate neighborhoods, an expanded neighborhood,
shortest-hop refinement, and a statistical stopping rule. The paper's CPU
experiments tune tile sizes; this experiment tunes tile size and innermost
unroll factor for Celox's code generator. The schedule is synthesized once by
our bounded scheduler and stays fixed throughout each search.

This is an implementation of that search approach with explicit engineering
choices, not a reproduction of the paper's reported benchmarks or a run of
external Pluto. The paper specifies three measurements and Mann–Whitney U but
does not specify a tail convention and significance threshold in its algorithm
description. We use a one-sided exact test with `p <= 1/20`. With three samples
in each group, the minimum attainable one-sided p is 1/20; a two-sided 5% exact
test could never accept an improvement.

## Implementation

[`celox-analysis::polyhedral::tuning`](../../crates/celox-analysis/src/polyhedral/tuning.rs)
has no runtime, clock, compiler, or external solver dependency. Its measurement
callback supplies costs or rejects a candidate. The search:

- Pools neighbors from every coordinate and selects the lowest-median candidate
  whose improvement over the incumbent passes the rank test. It does not
  exhaust one axis before considering another.
- Uses a configurable graph radius, two in this experiment. Refinement uses
  each coordinate's smallest coarse value as its step, within the coarse
  minimum/maximum. It can produce off-grid values such as unroll 31.
- Caches measurements and rejections, with separate limits on candidate
  evaluations and accepted moves. Defaults are 64 evaluations and 100 moves;
  the driver uses 48 evaluations. Exhaustion is distinguished from convergence.
- Computes exact rank-sum probabilities by counting label permutations with
  dynamic programming. Average ranks represent ties exactly using integers.
  There is no normal approximation or additional statistics package.

[`CodegenOptions`](../../crates/celox-sir/src/affine.rs) adds an unroll factor
bounded to 1–64 and an output instruction limit. Scan generation still verifies
the schedule independently. The
[`SIR emitter`](../../crates/celox-sir/src/affine/emit.rs) groups consecutive
innermost points in their original lexicographic order and emits an exact tail.
It never evaluates an out-of-domain lane speculatively. Existing endpoint
partitioning and guarded scanning handle statement boundaries and dynamic
prefixes. Unsupported arithmetic or excessive generated code rejects a
candidate.

Within each straight-line batch, proven affine subscripts are canonicalized
after substituting the actual induction coordinates. This exposes equal loads
across lanes; typed value numbering then reuses their expression DAG, including
four-state operations. Every store invalidates cached loads of its object.
Availability never crosses a backedge or control-flow boundary. Distinct
objects retain the adapter's non-aliasing requirement. Unrolling and sharing
were introduced together; their individual contributions have not been
isolated by an ablation experiment. No SIMD arithmetic improvement is claimed.

The [native experiment](../../crates/celox/examples/affine_tuning.rs) uses actual
Veryl map and producer/consumer inputs. It captures ordinary frontend SIR,
recovers eligible pre-optimization units, and compares candidates against the
fully optimized ordinary unit. Both sides use the existing merged-SIR
optimizer and x86 backend with packed state layout. The driver checks complete
semantic state against the baseline before and after candidate measurements.
It rejects generated SIR above 16,384 instructions or native code above 65,536
bytes; that restriction does not remove the ordinary baseline.

After search, the driver records a separate acceptance decision from fresh
measurements against the ordinary baseline. A lower median and the rank test
are both required to select `tuned`; otherwise it records `baseline`. This is
an experimental selection report, not an installation into the simulator.

## Measurement protocol, 2026-09-11

Host: AMD Ryzen 7 9800X3D, 96 MiB L3, Linux under a Microsoft hypervisor,
Rust 1.98.1, `heliodor-dev` profile, process pinned to CPU 2. Each invocation
measures map and producer/consumer in two-state mode, four-state mode with
zero masks, and four-state mode with pseudorandom payloads and masks. Known
and mixed-mask trials generate the same kind of code; this is not input-value
specialization.

Three separate processes were run for each of 32, 512, and 4,096 elements:
54 complete searches in total. Each search starts at `(tile=n, unroll=1)`.
The coarse tile grid is the sorted unique values of
`[16, 32, 64, 128, 256, 512, n]` no greater than `n`; the unroll grid is
`[1, 2, 4, 8, 16, 32]`. The full-extent seed gives the previously measured
untiled behavior while keeping the tiling structure fixed during search.

Each search candidate receives three timed batches after warmup. Each batch
contains `clamp(2_000_000 / n, 32, 100_000)` evaluations. The final comparison
uses 15 fresh batches per implementation, different input seeds, and alternating
baseline/candidate order. All output cells and masks must still match. The
inputs are immutable within these units, so repeated evaluations are stable.
These are warmed execution-unit timings, not full RTL application throughput.

The exact tests are pointwise screening rules. Cached adaptive comparisons,
multiple tested conditions, and temporal correlation in timings do not yield
a global 95% guarantee. In particular, three search samples sometimes stop at
an unroll factor of 1 or 4, while another process reaches 16 or 32. Fresh
validation protects the acceptance decision for the measured run; it does not
establish a portable optimum or guarantee a win on later workloads.

## Results

Ratios below are baseline median time divided by tuned median time from fresh
validation. A value above 1 is faster. The median and range summarize the three
separate process results, including unsuccessful candidates. The last column
counts how often the final comparison selected the tuned unit; a ratio alone
does not determine acceptance.

| Elements | Case | State / masks | Median ratio | Range | Tuned selected |
| ---: | --- | --- | ---: | ---: | ---: |
| 32 | map | 2-state known | 0.908 | 0.727–0.915 | 0/3 |
| 32 | map | 4-state known | 0.676 | 0.576–0.995 | 0/3 |
| 32 | map | 4-state mixed | 0.812 | 0.749–0.989 | 0/3 |
| 32 | producer/consumer | 2-state known | 1.024 | 0.974–1.027 | 2/3 |
| 32 | producer/consumer | 4-state known | 1.055 | 1.048–1.059 | 3/3 |
| 32 | producer/consumer | 4-state mixed | 0.929 | 0.720–1.056 | 1/3 |
| 512 | map | 2-state known | 0.527 | 0.526–0.527 | 0/3 |
| 512 | map | 4-state known | 0.994 | 0.981–1.027 | 1/3 |
| 512 | map | 4-state mixed | 0.997 | 0.989–1.000 | 0/3 |
| 512 | producer/consumer | 2-state known | 0.735 | 0.721–0.740 | 0/3 |
| 512 | producer/consumer | 4-state known | 1.079 | 1.079–1.132 | 3/3 |
| 512 | producer/consumer | 4-state mixed | 1.133 | 0.962–1.136 | 2/3 |
| 4,096 | map | 2-state known | 0.992 | 0.977–0.994 | 0/3 |
| 4,096 | map | 4-state known | 0.895 | 0.677–1.055 | 1/3 |
| 4,096 | map | 4-state mixed | 1.086 | 1.020–1.179 | 3/3 |
| 4,096 | producer/consumer | 2-state known | 1.046 | 0.734–1.203 | 2/3 |
| 4,096 | producer/consumer | 4-state known | 1.172 | 1.106–1.253 | 3/3 |
| 4,096 | producer/consumer | 4-state mixed | 1.162 | 1.085–1.365 | 3/3 |

All 54 searches converged within 5–19 candidate evaluations, with no legality
or size rejections. Fresh comparison selected 24 tuned units and 30 baselines.
Every search selected the full-extent tile. These measurements establish no
benefit from tile subdivision: the useful changes here are partial unrolling
with expression sharing and selecting parameters against the existing code.
Off-grid factors 5, 17, and 31 were reached at 32 elements; that demonstrates
refinement, not an independent performance advantage of shortest hops.

For the consistently improving four-state producer/consumer with known values:

| Elements | Baseline bytes | Tuned bytes across trials | Search ms | Approximate evaluations to amortize search |
| ---: | ---: | ---: | ---: | ---: |
| 32 | 4,371 | 3,927 | 1,427–1,792 | 476–618 million |
| 512 | 68,091 | 4,552–8,819 | 1,761–1,862 | 17–28 million |
| 4,096 | 543,867 | 4,552–8,819 | 1,769–1,910 | 1.1–2.6 million |

Amortization is `search_ms * 1000 / (baseline_us - tuned_us)` per trial, using
rounded CSV medians. It is a lower estimate of the total incremental work:
`search_ms` includes candidate generation, backend compilation, warmups and
search measurements, but excludes recovery/scheduling and the fresh final
comparison. Common frontend work and baseline backend compilation are reported
separately. They are still paid, so these data do not establish a reduction in
total compiler latency. The search range over all 54 conditions was 68–1,910 ms.

Raw CSVs and complete candidate/sample logs are in
[`affine-tuning-2026-09-11`](../benchmarks/data/affine-tuning-2026-09-11/).
[`summary.csv`](../benchmarks/data/affine-tuning-2026-09-11/summary.csv) groups the
three process trials without dropping failures. Each `n<N>-trial<T>.csv` has
a sibling `.txt` recording schedules, every candidate's costs and bytes,
accepted search trajectory, and final baseline/candidate sample arrays.

## Adoption implications

Preserving the existing optimized unit is necessary: all two-state map trials
and all 512-element two-state producer/consumer trials prefer it. The repeatable
four-state result justifies keeping this approach in consideration for larger,
frequently evaluated units. Running a second-scale search on every small unit
would be a poor default even where a few nanoseconds are saved per evaluation.

The next useful integration work is to retain array iteration and access
provenance before frontend expansion, then apply schedule/code-generation
selection only where expected evaluation count can repay its cost. Early SLT
integration is still missing; these experiments reconstruct eligible SIR after
already paying for unrolling and ordinary optimization. Scalar reductions,
general mutable-array recovery, layout aliases, and the FF shift fixture remain
outside this bridge's supported contract. The SLT adapter and canonical SIR
adapter continue to be tested, but ordinary SLT does not yet emit their array
regions automatically.

Search cost and stability also need work before adoption. Equivalent generated
candidates could be deduplicated, and slow tiled boundary code needs to be
understood before spending repeated measurements on it. A budget policy should
include validation cost and expected runtime savings, rather than selecting
only the best warmed kernel median. Representative RTL designs and other hosts
remain necessary before enabling any default path.

## Reproduction and validation

```sh
cargo build --offline --profile heliodor-dev -p celox --example affine_tuning
mkdir -p /tmp/celox-affine-tuning
for n in 32 512 4096; do
    for trial in 1 2 3; do
        taskset -c 2 target/heliodor-dev/examples/affine_tuning "$n" 15 48 \
            > "/tmp/celox-affine-tuning/n${n}-trial${trial}.csv" \
            2> "/tmp/celox-affine-tuning/n${n}-trial${trial}.txt"
    done
done

cargo test --offline -p celox-analysis -p celox-sir -p celox-slt --lib
cargo test --offline --profile heliodor-dev -p celox --test affine_veryl --test affine_scheduling
cargo test --offline --profile heliodor-dev -p celox --no-default-features --test affine_veryl --test affine_scheduling
cargo clippy --offline -p celox-analysis -p celox-sir --all-targets -- -D warnings
cargo clippy --offline -p celox --test affine_scheduling --test affine_veryl --example affine_tuning -- -D warnings
cargo check --offline -p celox --no-default-features --example affine_tuning
cargo fmt --all -- --check
```

Validation passed 272 crate unit tests, six native-enabled integration tests,
and the same six integration tests without default features. All 54 native
searches and their fresh comparisons passed semantic-state checks. Tests cover
partial and oversized unroll factors, tiled and untiled tails, negative
coordinates, disjoint statement domains, mutable-array dependencies, actual
Veryl boundaries and exceptions, known/X/Z values, and interpreter/native
agreement. The exact rank test is checked against independently enumerated
Mann–Whitney scores for all 729 six-observation datasets over three tied values.
Search tests cover pooled best-fit selection, a valley requiring radius two,
off-grid refinement, cache/rejection handling, and stopping budgets. Focused
Clippy, the interpreter-only example check, and workspace formatting also pass.
