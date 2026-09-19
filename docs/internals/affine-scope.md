# Affine coverage and two-candidate selection

This follow-up asks whether the benefits of [numeric tuning](affine-tuning.md)
survive a cheaper selection procedure and a wider range of Veryl inputs. It
adds verified affine input-map recovery, normalization of partial reads, a
two-candidate timing driver, and an audit of existing repository designs. All
implementation is local, with no new dependencies or compiler-default changes.

The result has two parts. Candidate generation and acceptance now take
17–66 ms in the measured cases, including correctness checks and fresh timing
validation. Coverage of the constructed Veryl families grows from 4/12 to 8/12,
and conditional array arithmetic provides an additional repeatable improvement.
However, none of the execution units in four audited repository designs meets
the current whole-unit extraction contract. Region extraction around control
flow and simulation effects is the next integration boundary.

## Recovery changes

[`recover_independent_stores`](../../crates/celox-sir/src/affine/unrolled.rs)
previously grouped expression DAGs after translating every input array index
by the output index. This required unit-stride input maps and rejected array
broadcasts, reverse order, and strided input pairs.

The new bridge groups typed expression topology separately from concrete load
indices. For each contiguous run of output indices, adjacent lanes propose an
integer affine map for each load. Every subsequent lane must match every map;
a mismatch starts a separate bounded piece. Holes remain separate domains,
singletons use constant maps, and the statement/work limits still apply.
Two observations propose a model, but never justify extrapolation over other
lanes. The ordinary kernel adapter then proves the emitted index arithmetic
does not wrap and stays within the declared object bounds.

Static reads contained within one cell can now be represented by a complete
cell load followed by the original slice. This handles conditions such as
`a[i][0]`, while preserving payload and X/Z bits. The existing proof requires
the entire input object to remain read-only within the unit. Cross-cell reads,
partial output writes, overlapping outputs, mutable input objects, indirect
loads, captures, notifications and commits remain rejected. The scheduler and
code generator do not acquire permission to reorder those effects.

## Constructed Veryl coverage

The source fixtures are in
[`scope_cases`](../../crates/celox/examples/affine_veryl_support/mod.rs).
The before/after audit uses 31 elements and both two- and four-state modes.
Results below are for pre-optimization SIR; both modes agree. The before run
used the previous recovery implementation (`cfe5d5a27`) with the new fixtures,
before modifying the bridge. The archived logs preserve both runs.

| Family | Before | After | Relevant boundary |
| --- | --- | --- | --- |
| Map | Yes | Yes | Unit-stride independent cells |
| Producer/consumer | Yes | Yes | Inlined producer expressions and boundary stores |
| FIR with five taps | Yes | Yes | Multiple shifted reads and arithmetic |
| Strided pair | No | Yes | `a[2*i]` and `a[2*i+1]` |
| Reverse | No | Yes | `a[n-1-i]` |
| Broadcast coefficients | No | Yes | Fixed reads from a multi-element coefficient array |
| Conditional lane arithmetic | No | Yes | One-bit read, arithmetic and four-state mux |
| Rotate by one | Yes | Yes | Affine interior and exceptional last lane |
| Scalar reduction | No | No | Carried scalar state / fold CFG |
| FF shift | No | No | State-application units and effects |
| Indexed gather | No | No | Data-dependent index and remainder |
| Alternating lane expressions | No | No | Noncontiguous output families exceed the piece limit |

This is a coverage test of deliberately chosen families, not an estimate of the
fraction of arbitrary RTL that can be optimized. Eligibility also says nothing
about profitability: the newly eligible reverse, broadcast, and strided-pair
cases lose to the existing implementation in every recorded timing condition.

## Two-candidate measurement

[`affine_scope`](../../crates/celox/examples/affine_scope.rs) uses unroll factors
16 and 32 with no tile subdivision. These choices were fixed from the preceding
experiment before timing the broader matrix. This driver is a separate, small
candidate policy; it does not replace or reproduce the coordinate hill-climbing
algorithm. It uses the same verified schedule and sharing-preserving code
generation.

Three training batches per candidate select the lower median. A fresh comparison
against the ordinary optimized unit then uses 15 batches per side, alternating
execution order and using different input seeds. Acceptance requires a lower
median and the same one-sided exact rank test (`p <= 1/20`). Complete stable
state, including internal objects and masks, must match before and after
training measurements and after each validation pair.

The baseline calibrates a batch toward 250 microseconds, bounded to 8–65,536
evaluations. This replaces the earlier fixed element-work count as well as
reducing the candidate set. Both changes contribute to the lower cost. Shorter
batches can be noisier; the data do not prove identical decisions to a full
search or to longer measurements. Pointwise tests and cached/training selection
also do not provide a global significance guarantee across this matrix.

The driver retains the ordinary frontend, all existing SIR optimizations,
packed layout, and x86 backend. It rejects layout aliases and generated units
above the existing 16,384-instruction / 65,536-byte experiment limits. These
are warmed execution-unit measurements, not whole-simulator throughput. Input
objects stay immutable during repeated evaluations. No SIMD speedup or
input-value specialization is claimed.

Host and profile are unchanged: AMD Ryzen 7 9800X3D, 96 MiB L3, Linux under a
Microsoft hypervisor, Rust 1.98.1, `heliodor-dev`, pinned to CPU 2. There are
three independent processes at each of 255 and 1,023 elements, covering all
eligible families in two-state, four-state known, and four-state mixed-mask
modes. These element counts were not used to choose the two candidates and
exercise partial tails. Separate controls repeat the original producer/consumer
at 512 and 4,096 elements. In total, 162 decisions were measured.

## Cost and execution results

`decision_ms` includes recovery/scheduling, both candidate compilations,
calibration, warmup, semantic comparisons, training, and final validation.
Recovery runs before baseline compilation, and its measured duration is added
to the subsequent decision interval. Common frontend work, state-layout
construction and the ordinary baseline backend compile remain separate.
Consequently, this is incremental selection cost, not total compiler latency.

Across all 162 decisions, that incremental cost is 16.786–65.875 ms, with a
median of 29.650 ms. Forty-six decisions select a generated candidate; the other
116 retain the ordinary optimized unit in the experiment's report.

For the 4,096-element, four-state producer/consumer with known inputs:

| Process trial | Baseline microseconds | Candidate microseconds | Throughput ratio | Decision ms |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 8.2121 | 7.0071 | 1.172 | 49.810 |
| 2 | 8.1622 | 6.7582 | 1.208 | 46.766 |
| 3 | 9.1030 | 7.1055 | 1.281 | 51.241 |

All three select unroll 32 and pass fresh acceptance. The baseline is 543,867
bytes and the candidate 8,819 bytes. The illustrative payback for incremental
selection is approximately 26,000–41,000 evaluations, computed per trial from
`decision_ms * 1000 / (baseline_us - candidate_us)`. The preceding full search
cost 1,769–1,910 ms for this condition, excluding its fresh validation. These
are separate process measurements, not a paired cost ablation.

At 512 elements, known four-state producer/consumer selects the candidate in
2/3 trials. The remaining trial has noisy samples and fails the rank test despite
a favorable median ratio. Reducing selection cost has not established the same
acceptance stability as the earlier longer measurements.

For the broader inputs, each entry below is the number of accepted generated
candidates out of three independent process trials:

| Elements | Family | 2-state | 4-state known | 4-state mixed |
| ---: | --- | ---: | ---: | ---: |
| 255 | Map | 0 | 0 | 0 |
| 255 | Producer/consumer | 0 | 2 | 3 |
| 255 | FIR, five taps | 0 | 0 | 1 |
| 255 | Strided pair | 0 | 0 | 0 |
| 255 | Reverse | 0 | 0 | 0 |
| 255 | Broadcast | 0 | 0 | 0 |
| 255 | Conditional lane arithmetic | 0 | 3 | 2 |
| 255 | Rotate | 0 | 0 | 0 |
| 1,023 | Map | 0 | 2 | 2 |
| 1,023 | Producer/consumer | 0 | 2 | 3 |
| 1,023 | FIR, five taps | 0 | 1 | 2 |
| 1,023 | Strided pair | 0 | 0 | 0 |
| 1,023 | Reverse | 0 | 0 | 0 |
| 1,023 | Broadcast | 0 | 0 | 0 |
| 1,023 | Conditional lane arithmetic | 3 | 2 | 2 |
| 1,023 | Rotate | 0 | 2 | 3 |

The new conditional-lane family improves 1.126–1.208x in all three known
four-state trials at 255 elements. Its two-state 1,023-element trials also
all pass, with median ratio 1.298x and range 1.294–2.145x; the unusually large
third ratio should not be treated as a stable effect size. Several apparent
wins in other conditions do not repeat. Full ratios, including failed
candidates, are retained in the data rather than filtering to accepted runs.

Artifacts: [all measurements and logs](../benchmarks/data/affine-scope-2026-09-11/),
[54-group summary](../benchmarks/data/affine-scope-2026-09-11/summary.csv),
[before audit](../benchmarks/data/affine-scope-2026-09-11/audit-31-before.csv),
and [after audit](../benchmarks/data/affine-scope-2026-09-11/audit-31-after.csv).
Each CSV has a sibling `.txt` with candidate choices and raw batch samples or
rejection reasons. The summary includes every trial and all three data modes.

## Existing repository designs

`affine_veryl repo` compiles four existing sources at their default parameters
and audits every execution-unit category before and after ordinary SIR
optimization. Both adapters reject every complete unit in both state modes:

| Design | Units per state mode / phase | Eligible complete units | Main observed rejection |
| --- | ---: | ---: | --- |
| 1,000 counters | 9 | 0 | Branch CFGs and state-application effects |
| Linear sorter, depth 8 | 37 | 0 | Branch CFGs and effects |
| Pull sorter, depth 100 | 405 | 0 | Branch CFGs and effects |
| AXI-Lite register file | 9 | 0 | Branch CFGs, effects, and scalar-state loops |

These counts include alternative evaluation/application unit categories and
are not dynamic execution weights. The empty `n` field in this audit means
the design's own defaults are used. The test does not search smaller regions
inside rejected units. For example, the counter and sorter IR contain commits
and reset/enable branches around array or repeated cell updates. They cannot
be treated as globally commuting stores by dropping their effects.

Sources are the repository's
[counter benchmark](../../crates/celox/testdata/veryl/top_n1000.veryl),
[linear sorter](../../crates/celox/tests/macro_project/src/linear_sorter.veryl),
[pull-sorter fixture](../../crates/celox/tests/fixtures/linear_sorter_pull_mre.veryl),
and [AXI-Lite fixture](../../crates/celox/tests/fixtures/bitslice/axi_lite_reg_file.veryl).
The [audit CSV](../benchmarks/data/affine-scope-2026-09-11/repository-audit.csv)
and [rejection log](../benchmarks/data/affine-scope-2026-09-11/repository-audit.txt)
record both phases and both state modes.

The [subsequent region experiment](affine-regions.md) now extracts and verifies
smaller regions in the counter and AXI designs, and measures a complete counter
clock unit. The whole-unit results above remain unchanged.

The next adoption priority from this audit was extracting useful subregions while
preserving their live values, branch predicates, and ordered effects. Earlier
SLT retention of array iteration/access provenance is still needed to avoid
paying for expansion first. This change extends the SIR bridge; it does not
automatically discover array regions in ordinary SLT, support scalar reductions,
or establish an application-level speedup. The current evidence favors
investigating those boundaries before adding more numeric search dimensions.

## Reproduction and validation

```sh
cargo build --offline --profile heliodor-dev -p celox --example affine_scope --example affine_veryl
taskset -c 2 target/heliodor-dev/examples/affine_veryl scope 31
taskset -c 2 target/heliodor-dev/examples/affine_veryl repo
mkdir -p /tmp/celox-affine-scope
for n in 255 1023; do
    for trial in 1 2 3; do
        taskset -c 2 target/heliodor-dev/examples/affine_scope "$n" \
            > "/tmp/celox-affine-scope/n${n}-trial${trial}.csv" \
            2> "/tmp/celox-affine-scope/n${n}-trial${trial}.txt"
    done
done
for n in 512 4096; do
    for trial in 1 2 3; do
        taskset -c 2 target/heliodor-dev/examples/affine_scope "$n" producer_consumer \
            > "/tmp/celox-affine-scope/control-n${n}-trial${trial}.csv" \
            2> "/tmp/celox-affine-scope/control-n${n}-trial${trial}.txt"
    done
done

cargo test --offline -p celox-analysis -p celox-sir -p celox-slt --lib
cargo test --offline --profile heliodor-dev -p celox --test affine_scheduling --test affine_veryl
cargo test --offline --profile heliodor-dev -p celox --no-default-features --test affine_scheduling --test affine_veryl
cargo clippy --offline -p celox-sir --all-targets -- -D warnings
cargo clippy --offline -p celox --test affine_scheduling --test affine_veryl --example affine_scope --example affine_veryl --example affine_tuning -- -D warnings
cargo check --offline -p celox --no-default-features --example affine_scope
cargo fmt --all -- --check
```

Validation passed 273 crate unit tests, seven integration tests and the same
seven without default features. The expanded differential tests cover all eight
eligible families, byte swapping via nonzero partial-read offsets, a late-lane
exception, sizes 3/7/19, unroll 2/7/32, tiled/untiled tails, and known/X/Z data.
Known results have independent Rust oracles; all 19-element variants also check
native state. Core tests check positive/negative/zero input strides, late
exceptions under a one-statement budget, holes, and rejection of cross-cell
reads, partial writes, overlapping writes, mutable inputs and effects. All 162
native selection experiments passed their semantic comparisons. Focused Clippy,
the non-native example check and workspace formatting also pass.
