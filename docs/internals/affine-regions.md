# Affine regions inside existing execution units

The [whole-unit audit](affine-scope.md#existing-repository-designs) rejected all
four existing designs. Extracting smaller regions now finds legal candidates
in the counter benchmark and AXI-Lite register file. The counter's complete
clock execution unit also shows a repeatable four-state throughput improvement,
provided its reset fill is left to the ordinary optimizer. This remains an
opt-in experiment, with no new dependencies or default compiler changes.

## Extraction and replacement contract

`celox_sir::affine::recover_independent_regions` scans maximal straight-line
instruction spans within each basic block. It treats commits, observable stores,
events, captures, potential traps, indirect accesses and unsupported memory
ranges as boundaries. A candidate must contain at least two complete stores,
have no SSA live-outs, and have only immediate constants as external SSA inputs.
Constants are rematerialized with their original type and mask. Loads from an
earlier block or from before a commit are never rematerialized as current loads.

The existing recovery bridge proves immutable inputs and disjoint output cells
within each span, then derives and verifies its affine accesses. Objects may be
written outside the span: none of the transformed operations move across a
boundary. Scalar recurrences, general live-ins and regions spanning branches
remain unsupported. Rejected spans keep their original code.

The proof owns an immutable source snapshot. Lowering schedules selected kernels,
generates loops, allocates fresh register/block IDs, and rejoins the original
continuation. Later spans are spliced first so instruction ranges remain valid.
Original parameters, branch arguments, effects and terminators are retained;
the final unit is independently verified. Empty selections return the original
unit; invalid selections and generation failures cannot mutate the source.

The default discovery bounds are 100,000 instructions/registers, 1,024 blocks,
64 candidate attempts (including rejections), and the existing two-million-work
budget per recovery attempt. Scheduling/scanning have separate per-region work
bounds. The code-generation instruction budget also limits the sum of generated
instructions across selected regions. These are bounded experiments, not an
unrestricted symbolic polyhedral compiler.

## Existing-design coverage

The table reports each state mode separately; both produced these counts before
ordinary SIR optimization. All four still have zero eligible **whole** units.

| Design | Units | Units with regions | Regions | Stores inside regions |
| --- | ---: | ---: | ---: | ---: |
| 1,000 counters | 9 | 6 | 12 | 12,000 |
| Linear sorter, depth 8 | 37 | 0 | 0 | 0 |
| Pull sorter, depth 100 | 405 | 0 | 0 | 0 |
| AXI-Lite register file | 9 | 6 | 12 | 132 |

These counts include alternative evaluation/application categories and both
clock/reset events. They are neither dynamic weights nor a percentage of RTL
coverage. One counter clock unit has two regions: reset and increment. The
AXI regions include array updates inside otherwise unsupported control flow;
this does not make its scalar-state loops eligible.

After ordinary SIR optimization, recovery finds zero regions in these designs:
packed multi-cell stores and SSA sharing violate this bridge's narrower input
contract. This favors insertion before that optimization stage. The sorter
rejections include mutable inputs and SSA boundaries; four large pull-sorter
units hit the discovery limits. The audit does not prove that no other region
extraction algorithm could optimize them.

The [audit CSV](../benchmarks/data/affine-regions-2026-09-11/repository-audit.csv)
and [diagnostics](../benchmarks/data/affine-regions-2026-09-11/repository-audit.txt)
record all phases, state modes, counts and discovery times. Discovery across
all counter alternatives cost 21–23 ms in this audit; recovering the single
clock unit in the timing experiment generally cost 2–4 ms.

## Counter throughput and selection cost

The experiment uses the repository's unmodified
[1,000-counter design](../../crates/celox/testdata/veryl/top_n1000.veryl), its
ordinary optimized clock `eval_apply_ffs` unit as baseline, and the ordinary
packed memory layout. Timings execute the entire unit, including reset dispatch
and state commits, rather than just an extracted arithmetic loop. They still
exclude simulator event dispatch and other execution units.

Each candidate re-enters the **complete ordinary SIR optimization pipeline**
before the same merged-unit optimization and x86 backend used for the baseline.
The prototype clones the pre-optimization program, replaces one clock unit,
and reoptimizes the entire program to retain its whole-program memory facts.
This cost is included; it is not yet an efficient production integration.
Reusing only the native post-merge passes was insufficient to preserve the
baseline optimization of untouched spans, so those pilot timings are not the
measurements reported here.

Two policies each try unroll factors 16 and 32. `all` replaces both regions;
`readers` replaces only kernels with a memory read, leaving the constant fill
unchanged. This is a small experimental selection rule, not a general cost model.
There is no tile search. Three training batches pick one factor, followed by 15
fresh alternating baseline/candidate batches. Each batch starts with identical
state and performs identical numbers of state transitions. An exact one-sided
rank comparison at 5% and a faster median are required for acceptance. The
comparisons are pointwise, without a multiple-experiment significance guarantee.

The default `readers` policy produced these results over three process runs:

| State / path | Accepted / 3 | Speedup (baseline time / candidate time) | Incremental decision cost |
| --- | ---: | ---: | ---: |
| Two-state, reset | 0 | 0.995–1.002 | 84–93 ms |
| Two-state, increment | 0 | 0.628–0.664 | 83–106 ms |
| Four-state known, reset | 2 | 1.031–1.037 | 65–73 ms |
| Four-state known, increment | 3 | **1.141–1.197** | **65–69 ms** |
| Four-state mixed, reset | 0 | 0.999–1.039 | 69–75 ms |
| Four-state mixed, increment | 3 | **1.193–1.414** | **67–75 ms** |

The ratio is baseline time divided by candidate time; above one is faster.
The four-state increment decisions all chose unroll 32. Native code shrank from
75,804 to 27,772 bytes. Incremental decision cost includes recovery, both
candidates' complete SIR optimization and native compilation, initialization,
interpreter/native semantic checks, calibration, training and fresh validation.
It excludes the already-paid common frontend/layout and baseline compilation.

Using the measured time saved per call, known-value increment paths amortize
that extra cost after approximately **458k–624k calls**; mixed initial values
need **247k–459k calls**. Reset's small measured changes do not justify tuning
on their own: the two accepted reset runs need over eight million calls to
amortize selection. Preserving a source span does not force identical native
register allocation or surrounding code.

Replacing both regions is much worse for reset: `all` has ratios 0.350–0.481,
and none of its nine reset decisions accepts the candidate. Two-state increment
also always keeps the baseline. The `all` four-state increment results are less
consistent (known 3/3 accepted; mixed 2/3), with substantial timing variation.
Legal region recovery therefore needs both path-aware selection and a complete
ordinary optimization pipeline. It cannot simply enable every recovered loop.

Across both policies there are 36 decisions, 13 accepted and 23 retaining the
baseline. Results are conditional on a fixed reset/increment path; the driver
does not install a policy or estimate a real workload's path frequencies.
Mixed runs initialize payload and X/Z masks, then evolve that state; arithmetic
can quickly make entire words unknown. They do not draw fresh masks every call.
This warmed, CPU-pinned microbenchmark on the same Ryzen 7 9800X3D VM used in
the earlier reports is not an application-level or SIMD-speedup claim.

Raw data, batch timings and the aggregate table are in
[affine-regions-2026-09-11](../benchmarks/data/affine-regions-2026-09-11/summary.csv).
The remaining integration priorities are a hot-unit gate and economical reuse
of per-unit optimization context, plus retaining SLT array provenance before
unrolling. AXI has semantic coverage here but no native throughput measurement.

## Validation and reproduction

Differential tests compare all recovered counter/AXI execution-unit alternatives
against their original units using complete logical state and ordered commit,
trigger and capture observations. Tests cover both state modes, mixed masks,
reset and active paths, and unroll factors 7/32. A separate counter check runs
the full ordinary optimization pipeline and compares native execution with the
interpreter and a Rust wrapping-add/reset oracle, including overflow and a
sequence of reset encodings. Native execution first checks disjoint physical
ranges of all referenced objects; unrelated aliases elsewhere in the design
are neither initialized nor used as independent oracle outputs. The benchmark
also compares the full stable native memory image against the baseline.

Synthetic tests exercise multiple spans in one branch, imported constants,
commit-modified inputs between spans, branch arguments, event/capture ordering,
and selecting only some regions. Core tests reject snapshot live-ins, SSA
live-outs (including terminator arguments), block parameters, invalid region
selections, exhausted budgets and overflowing fresh IDs. The source remains
usable after rejection.

```sh
cargo build --offline --profile heliodor-dev -p celox --example affine_regions --example affine_veryl
taskset -c 2 target/heliodor-dev/examples/affine_veryl repo
for trial in 1 2 3; do
    for policy in all readers; do
        taskset -c 2 target/heliodor-dev/examples/affine_regions "$policy" \
            > "/tmp/regions-${policy}-${trial}.csv" \
            2> "/tmp/regions-${policy}-${trial}.txt"
    done
done
cargo test --offline -p celox-analysis -p celox-sir -p celox-slt -p celox-sir-opt --lib
cargo test --offline --profile heliodor-dev -p celox --test affine_scheduling --test affine_veryl --test affine_regions
cargo test --offline --profile heliodor-dev -p celox --no-default-features --test affine_scheduling --test affine_veryl --test affine_regions
cargo clippy --offline -p celox-sir --all-targets -- -D warnings
cargo clippy --offline -p celox --test affine_regions --test affine_scheduling --test affine_veryl --example affine_regions --example affine_veryl --example affine_scope --example affine_tuning -- -D warnings
cargo check --offline -p celox --no-default-features --example affine_regions --example affine_veryl
cargo fmt --all -- --check
```

All 682 crate unit tests passed, as did all ten integration tests with default
features and all ten without them. Focused Clippy, the portable example checks,
formatting, and all 36 native decision comparisons passed. No full simulator
workload speedup or native AXI measurement is claimed.
