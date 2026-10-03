# Optimization Architecture

Celox optimizes at three compiler layers and once more in the runtime. Each layer
uses the representation that can prove its transformations without depending on
details from adjacent phases.

```text
SLT structure
    ▼
SIR control flow and state accesses
    ▼
backend-private machine IR
    ▼
runtime event scheduling
```

User-facing optimization presets and trade-offs are documented in
[Optimization Tuning](/guide/optimization-tuning). This page describes where
the mechanisms live.

## Symbolic logic layer

SLT optimizations operate while RTL expressions and bit ranges are still
explicit.

- **Hash consing** interns structurally identical expressions so common logic has
  one symbolic identity.
- **Topological hoisting** materializes shared subexpressions once at a legal
  dependency point.
- **Range atomization** splits state at observed bit boundaries, avoiding
  whole-value work when only a slice is required.
- **Cost-directed mux lowering** chooses between eager selection and control flow
  before expression structure is lost.

This layer may use design dependency facts, but it does not know physical memory
offsets or target instructions.

## SIR layer

`celox-sir-opt` owns backend-independent transformations over execution units and
their control-flow graphs. The main families are:

| Family | Purpose |
|---|---|
| Forwarding and coalescing | Reuse known state values and combine adjacent bit-range accesses |
| CFG simplification | Fold proven branches, remove unreachable blocks, and simplify block arguments |
| Value simplification | Eliminate duplicate expressions, redundant concatenations, and algebraic identities |
| Commit optimization | Avoid unnecessary Stable/Working copies and sink commits toward their producers |
| Dead state removal | Remove writes that are not observable under the selected preservation policy |
| Scheduling | Reorder independent work while preserving data and memory dependencies |
| Layout requirements | Record proven state aliases for validation during physical layout |

Passes exchange analysis results through SIR- and design-owned identities. They
must not inspect Veryl syntax, assume x86 instruction costs as correctness facts,
or mutate a finalized physical layout.

Some transformations are ordered deliberately. For example, control-flow and
forwarding passes can expose dead values, while coalescing may create wider
operations that a later target either keeps or splits. The pass manager owns this
ordering; individual passes should not invoke one another as hidden pipelines.

## Physical layout boundary

Optimization may prove that two semantic state homes can share storage, but it
does not assign byte offsets. It emits a layout requirement, and
`celox-state-layout` validates width, state representation, and region
compatibility before applying the alias.

This separation keeps semantic rewrites independent of packing decisions and
prevents a backend from changing addresses after code generation begins.

## Native machine layer

The x86-64 backend lowers SIR into a private word-level SSA machine IR. At this
point it can use target facts that would be inappropriate in SIR, including:

- immediate instruction forms and x86 operand constraints;
- constant folding and algebraic simplification at machine width;
- copy propagation, global value numbering, and dead-code elimination;
- branch simplification and if-conversion;
- known-bits reasoning and redundant-mask elimination;
- target instruction selection for operations such as population count or bit
  extraction;
- pressure-aware scheduling, spilling, and register allocation.

Cranelift performs the corresponding target optimization through its own IR and
pass pipeline. WebAssembly generation likewise remains backend-private. No
machine-level result is written back into SIR.

## Runtime layer

Runtime optimizations avoid invoking compiled work when event semantics prove it
unnecessary:

- an event that does not match the required edge does not evaluate its domain;
- a non-cascaded single domain may use a combined evaluate-and-commit kernel;
- cascaded evaluation continues only while newly triggered domains exist.

These choices preserve the ordering model defined in
[Runtime Semantics](./cascade-limitations.md). They do not change signal values or
relax simulation consistency.

## Correctness boundary

Every compiler transformation must preserve observable state, events, errors,
and four-state value/mask behavior. A performance result can justify enabling or
disabling a legal transformation, but it is never evidence that an otherwise
unproven rewrite is correct.

## Experimental native parallel execution

On Linux x86-64, two-state native compilation can also emit a partitioned
alternative to the optimized serial fused kernel. Set `CELOX_PARALLEL=auto`
when compiling to include both alternatives. Ordinary compilation without this
setting remains serial. A precompiled image must contain the alternative before
runtime selection can use it; changing the environment cannot add code to an
existing serial image.

The scheduler partitions the dependency graph mechanically. It does not use
instance names, source hierarchy, or a workload-specific owner file. SCCs and
individual FF lowering actions remain atomic. Observable effects retain their
serial position. The native emitter returns a physical read/write footprint
from its final allocated MIR, including its private spill/scratch/save arena. Only functions
with complete footprints can share a concurrent wave. Unsupported addressing
or effects force singleton waves; pre-codegen SIR ranges cannot authorize
concurrency. Earlier experimental image markers are not accepted as evidence
of this check. A large design can therefore still have little usable parallelism.

Partition boundaries can extend the lifetime of intermediate publications.
Identity aliases are revalidated against partition readers: a value read by a
partition keeps its own storage instead of sharing a source that may already
have changed. Local publication elimination also accounts for physical aliases;
equivalent scalar homes are canonicalized before native instruction scheduling.

The correctness argument has separate obligations:

| Boundary | Required invariant |
|---|---|
| Scheduling | Every dependency edge keeps its order; SCCs and observable effects are indivisible. |
| Publication lowering | A value crossing a group boundary remains available until its last reader; aliases cannot shorten its lifetime. |
| Native emission | The footprint covers final machine-width loads, stores, read-modify-write operations, and emitter-owned spill/scratch/save storage. |
| Concurrent wave | Every pair has disjoint write/write and write/read ranges; an absent footprint or observable effect forces a singleton. |
| Runtime | Every participant completes the wave before the next wave starts; private trigger bits are merged only by the calling thread. |

Wave construction preserves the generated groups' serial order and only
combines adjacent, proven-independent groups. Thus its argument is that these
groups commute, separately from the scheduler's argument that the group order
preserves HDL semantics. A matching application run is a regression check, not
a substitute for either argument.

The current emission certificate admits direct scalar/SIMD memory operations
and an explicit list of register/control operations. Indexed/pointer accesses,
sparse memory pseudos, error exits, and unrecognized instruction kinds have no
certificate. Their groups execute alone. Certificates are collected after
allocation and its cleanup, not from the pre-allocation alias model. The
emitter's arena extent also covers out-of-SSA stack copies and scratch accesses
that do not appear as ordinary MIR memory instructions. Extending the admitted
instruction set requires reviewing its emission contract first.

The current runtime hook covers native fused evaluation used by batched initial
testbench execution. Ordinary `Simulator::tick`, four-state execution, tracing,
and other backends do not acquire parallel execution through this setting.

| Environment variable | Meaning |
|---|---|
| `CELOX_PARALLEL=off` | Execute the optimized serial alternative |
| `CELOX_PARALLEL=auto` | Compare serial execution and eligible worker counts at runtime |
| `CELOX_PARALLEL=N` | Request exactly N threads, including the calling thread |
| `CELOX_PARALLEL_PARTITIONS=N` | Compile-time lane budget, clamped to 1–64 |
| `CELOX_PARALLEL_PIN=1` | Pin threads, preferring distinct physical cores before SMT siblings |
| `CELOX_PARALLEL_CPUS=0,2,4,6` | Explicit ordered CPU list within inherited affinity; also enables pinning |
| `CELOX_PARALLEL_DIAGNOSTICS=1` | Emit selection and pool diagnostics to stderr |
| `CELOX_PARALLEL_WINDOW=N` | Maximum real evaluation calls per calibration sample (default 2048) |
| `CELOX_PARALLEL_WARMUP=N` | Real calls excluded from the rate sample after each calibration transition (default 64; 0 disables) |
| `CELOX_PARALLEL_COOLDOWN=N` | Calls before periodically checking the choice again (default 1000000) |
| `CELOX_PARALLEL_MAX_WORKERS=N` | Additional limit on auto candidates |
| `CELOX_PARALLEL_HOLD_SLOWDOWN_PERCENT=N` | Optional sustained-slowdown reprobe; default 0 disables it |

Pinning is opt-in and also pins the calling thread. Its affinity remains pinned
after the simulator is dropped; an embedding application that needs its original
mask must restore it explicitly.

Auto considers the available CPUs and concurrent wave width, rather than a
fixed four-thread ceiling. It can retain serial execution, rejects unstable
calibration samples, and periodically repeats calibration. Synchronization,
imbalance, SMT, and competing host load can make additional workers slower.
Workers use spin barriers with a scheduler yield after every 256 unsuccessful
polls, so total CPU time can rise even when wall time falls. Yielding lets a
descheduled participant run under contention; it does not make oversubscribed
execution or SMT profitable by itself.
Calibration and warmup advance the simulation; they do not replay input or
duplicate circuit state. End-to-end execution time includes all calibration,
warmup, pool creation, and synchronization costs. Short rate samples can still
choose a suboptimal width, so fixed-width comparisons remain necessary.
If a parallel trial already consumes 120% of the preceding complete serial
window, it ends early: finishing that window cannot show a speed gain within
the accepted drift. Its cost is normalized using the actual completed call
count. This bounds wasted sampling time on slow spin pools without replaying
cycles or crediting an incomplete window as a fast result.
These controls are experimental; static partition costs are scheduling
heuristics, not predictions of speedup or hardware clock frequency.

Compilation retains both serial and parallel IR/code, so its time and peak
memory must be measured separately from execution. Partitioning uses linear
graph storage; native private scratch arenas are packed using their generated
sizes. Tiered promotion reserves bounded additional parallel headroom and
declines promotion if the resulting image cannot fit its fixed allocation.
Compiler concurrency has its own CPU/memory budget and is independent of the
requested simulation thread count. Increasing execution threads does not
request the same number of concurrent compiler tasks.

For repeatable experiments, `scripts/benchmark-parallel.py` compiles serial and
dual images once, then runs repeated configurations in reversed order. It
records execution phases, peak RSS, host load, topology, actual thread affinity,
and image hashes, and compares output, event drain ticks, and top-level state.
`scripts/generate-parallel-benchmark.py` creates flat or hierarchical synthetic
arithmetic inputs without workload-specific partition hints.

### Local measurements

These runs used a Ryzen 7 9800X3D (8 physical cores, 16 logical CPUs), Linux
under WSL2, Rust 1.99.0, and O2 native images generated before execution. The
runner used the optimized `heliodor-dev` profile without host LTO. The source
snapshot and runner hash were frozen before measurement; the runner SHA-256 was
`bc9d6e49f959d00c7ee3bebcdd67655ac5b72d7933c95a09c14f550c318f23aa`.
Linux-reported physical cores were assigned before SMT siblings, with kernel
affinity readback. This verifies guest CPU affinity, not Windows hypervisor
placement. Host load and actual thread masks were recorded during each trial.

The synthetic inputs have 32 lanes and 64 arithmetic stages per lane. The
hierarchical input also has ring dependencies between lanes; this is not a
comparison of hierarchy alone. Each input ran for 1,000,001 ticks twice, with
the configuration order reversed in the second round. Values below are
end-to-end execution seconds, excluding code generation and image loading,
and including auto calibration, warmup, worker creation and synchronization.
Output, event drain ticks and top-level state matched in all 16 trials per input.

| Mode | Independent flat input | Hierarchical input with ring dependencies |
|---|---:|---:|
| Separate optimized serial image | 1.367 / 1.377 | 1.379 / 1.379 |
| Serial alternative in dual image | 1.360 / 1.365 | 1.406 / 1.460 |
| Partitioned, 1 thread | 1.433 / 1.310 | 1.670 / 1.683 |
| Partitioned, 2 threads | 1.356 / 1.328 | 1.641 / 1.755 |
| Partitioned, 4 threads | 0.951 / 0.917 | 1.348 / 1.339 |
| Partitioned, 8 threads | 0.833 / 0.814 | 1.333 / 1.469 |
| Partitioned, 16 threads | 5.932 / 1.326 | 3.062 / 2.700 |
| Auto | 0.936 / 0.970 | 1.396 / 1.400 |

Fixed 8-thread execution was fastest for the independent input in both rounds.
Auto selected 4 threads because its short calibration samples did not show an
8-thread improvement. For the coupled input, the small and inconsistent gains
from fixed 4/8 threads did not meet the default 10% threshold; auto selected
serial in both rounds. Auto is a bounded online heuristic, not a guarantee of
the best whole-run width. SMT workers were slower than the best physical-core
configuration, and the 16-thread flat result varied from 1.326 to 5.932 seconds.
Peak process RSS across synthetic trials was 20.4–20.7 MiB.

A calibration-length comparison on the independent input reused the same
images. With `CELOX_PARALLEL_WINDOW=8192` and `CELOX_PARALLEL_WARMUP=2048`,
auto chose 8 threads in both rounds and took 0.845 / 0.876 seconds, against
paired serial controls of 1.384 / 1.398 seconds. Increasing them further to
16384 / 8192 selected 4 then 8 threads and took 1.159 / 0.926 seconds.
Longer profiling is useful when startup dominates a short sample and the run
can amortize calibration, but more samples alone do not eliminate host variation.
The defaults remain the cheaper 2048 / 64 setting; they are not claimed to be
universally optimal. Fixed-width execution remains available for measured inputs.

For the public [Heliodor](https://github.com/dalance/heliodor/tree/6285682fa0a514077da9d17fee385c7841160025)
`test_soc_smp_linux_boot_8hart` input, the final dual-image compilation took
614.24 seconds wall time and 662.41 seconds CPU time, with 7.69 GiB peak process
RSS. It ran with an 8 GiB cgroup memory limit and no swap; no limit-hit or OOM
events were recorded. Partitioning produced 2,351 groups and 1,777 ordered
waves, including 1,573 singleton waves; maximum concurrent width was 16.
The partition count alone therefore substantially overstates useful parallelism.

The 30,000-tick prefix was run twice in reversed configuration order using
that same precompiled dual image and a separately optimized serial image.
All 16 trials matched in output, event drain ticks and top-level state.
The standalone serial image was reused from the earlier baseline generation;
the later changes affect partitioning, parallel emission and runtime selection,
not the standalone serial code path. These are execution seconds:

| Heliodor mode | Round 1 | Round 2 |
|---|---:|---:|
| Separate optimized serial | 3.402 | 3.636 |
| Serial alternative in dual image | 3.521 | 3.525 |
| Partitioned, 1 thread | 13.759 | 13.924 |
| Partitioned, 2 threads | 14.120 | 14.385 |
| Partitioned, 4 threads | 15.252 | 15.284 |
| Partitioned, 8 threads | 17.196 | 16.598 |
| Partitioned, 16 threads | 39.851 | 36.153 |
| Auto | 4.812 | 4.917 |

Serial was fastest. Auto selected the optimized serial alternative, but its
calibration cost was substantial on this short prefix. Fixed single-thread
partitioned execution was also slower than optimized serial, so thread
synchronization alone cannot explain the difference. Partition boundaries
change the generated code and optimization scope. Peak process RSS ranged
from 621.2 to 646.0 MiB across these trials, including 16-thread execution;
the simulator does not duplicate the full circuit state for each worker.

A longer 1,200,000-tick prefix compared serial and auto twice, reversing
order in the second round. All four runs matched, including console events
and their drain ticks. Auto selected serial both initially and after the
1,000,000-call cooldown in each run:

| Mode | Round 1 (s) | Round 2 (s) |
|---|---:|---:|
| Separate optimized serial | 142.581 | 137.192 |
| Auto | 148.214 | 147.515 |

The auto/serial difference includes calibration and the dual image's serial
alternative; it is not a pure barrier-cost measurement. The reverse-order
serial control also changed by about 4%, so a single timing should not be
interpreted as an exact stable rate. Interval records show where calibration
costs occur. The following are microseconds per tick in 100,000-tick windows
for round 1:

| Tick interval | Serial | Auto |
|---|---:|---:|
| 0–100,000 (initial calibration) | 117.73 | 132.00 |
| 100,000–1,000,000 (held choice) | 118.13–119.80 | 120.68–123.64 |
| 1,000,000–1,100,000 (recalibration) | 119.89 | 132.35 |
| 1,100,000–1,200,000 | 119.26 | 121.83 |

These are RTL execution measurements, not a physical timing/Fmax model.
They do not establish full Linux-boot correctness or five-hour steady-state
speedup. Earlier instance-name-based experiments are not evidence for this
generic partitioner.
