# Parallel Simulation

Celox can evaluate one design on several threads. Compilation splits the
combinational settle and the sequential update of each clock event into
*lanes*, one per thread, and the lanes run concurrently while every result
stays identical to sequential simulation.

## Enabling threads

```rust
use celox::Simulator;

let mut sim = Simulator::builder(code, "Top")
    .threads(4)
    .build()?;
```

```typescript
const sim = await Simulator.create(module, { threads: 4 });
```

`threads` counts the calling thread: `threads(4)` starts three worker threads.
The default, `1`, compiles only the sequential kernels.

## When it helps

Parallel execution pays off when one clock cycle contains a lot of
independent work, for example many instances of a datapath, wide replicated
logic, or large banks of registers. Each cycle has a fixed synchronization
cost of roughly a few hundred nanoseconds, so small designs whose cycle
already takes about a microsecond or less do not get faster.

Celox protects you from slowdowns in two ways:

1. **At compile time**, a phase is partitioned only when the estimated
   speedup outweighs the synchronization it needs. Otherwise that phase keeps
   its sequential code.
2. **At run time**, every partitioned phase is measured against its
   sequential version during the first calls, and the faster one is kept.
   The choice is measured again at intervals that double each time, so a
   choice made while the machine was busy is corrected later.

The measurement is part of the normal simulation, so it does not change any
result. Each measurement costs a few dozen calls of the slower version.

## Choosing a thread count

- Use at most the number of **physical** cores. Lanes wait for each other by
  spinning, so SMT siblings and oversubscribed cores slow everything down.
- Measure the complete workload with the same inputs and cycle count, as with
  [optimization levels](./optimization-tuning.md).
- More threads only help while each lane still has enough work per cycle.

## What runs in parallel

| Phase | Partitioned |
|---|---|
| Combinational settle (`eval_comb`) | Yes |
| Sequential update of one clock or reset event | Yes |
| Simultaneous or cascaded clock domains | Sequential |
| Native fused comb + FF tick loop | Replaced by the partitioned phases when they are faster |

Runtime events such as `$display` and assertions, and stores that can
trigger clock events, stay ordered exactly as in sequential simulation.

## Backends

Parallel execution is available on the native x86-64 and AArch64 backends
and on Cranelift. The WebAssembly backend and the interpreter always run
sequentially. Native force support for foreign interfaces keeps sequential
execution.

## Memory layout

Each lane's state lives in its own memory segment, aligned to cache lines and
separated by guard bytes, so lanes never write to the same cache line except
for objects that several lanes write by design (for example, different
elements of one array). See [Parallel Execution](/internals/parallel-simulation)
for the design.

## Inspecting partitioning and execution

Generate an offline, interactive HTML report (no server or network required):

```sh
cargo run --locked --release -p celox-bench --bin parallel-report -- \
  --output work/parallel-auto
cargo run --locked --release -p celox-bench --bin parallel-report -- \
  --always --output work/parallel-forced
```

The built-in workload contains 16 independent clocked datapaths. The automatic
run shows the normal cost model and measured fallback. `--always` exposes the
available parallel implementation even when it is slower. Open `report.html`
in each output directory; `report.json` retains the underlying records.

The report includes:

- Source dependency graphs down to individual LogicPaths and SLT expressions.
  Click an atomic item to inspect its paths, source signals, ownership group,
  lane-0 constraints, shared-expression merges and lowering into SIR units.
- Every item/subtree candidate's lane assignment and estimated cost, plus the
  selected assignment and acceptance threshold. FF part costs, locality votes,
  ownership and placement decisions are available in the event inspector.
- Dependencies recomputed from optimized SIR and, for native backends, from
  final compiled task effects. Select a node to inspect RAW/WAR/WAW conflicts,
  state bit ranges (`StateBits`), widened write envelopes (bytes), and shared
  runtime resources.
- Each captured dispatch, task duration and individual dependency wait/check,
  with arrows to producer tasks, measured overlap and ordering checks. Click a
  task for its merged SIR inputs and signal names. The SIR listing precedes
  native instruction-selection rewrites; the final footprint graph reflects
  those rewrites. Final native SIR is included in task details as well. IDs are local to their phase, and the view keeps those phases
  separate rather than assuming optimizer unit indices preserve source IDs.
- Measured selector medians and decisions in the event inspector. Their records
  are captured during untraced warmup as well. A fused sequential fast path can
  bypass individual selector calls and produce no lane dispatch.

Run on a self-contained Veryl design with constant inputs and an explicit clock:

```sh
cargo run --locked --release -p celox-bench --bin parallel-report -- \
  --source design.veryl --top Top --clock clk --threads 4 \
  --set enable=1 --observe result --observe count --backend cranelift \
  --warmup 256 --cycles 1000 --capture 8 --output work/my-scheduler
```

Repeat `--source`, `--set` and `--observe` as needed. Input integers are decimal
u64 values. This runner repeatedly triggers the specified event with `tick`;
it does not run an arbitrary testbench, change inputs per cycle, or synthesize a
reset sequence. Supply observations appropriate to that stimulus. Without
`--observe`, custom designs are timed but functional equivalence is not checked.
The demo checks all 16 outputs against a separately compiled one-thread build
after each captured tick. These checks cover the specified stimulus, not every
possible behavior of a design.

The speedup measurement uses the median of six alternating untraced blocks,
after warmup. Use release builds for performance conclusions. Task tracing
adds allocation and timestamp overhead and can change scheduling; its timeline
is for debugging. A recorded overlap is overlapping wall-clock intervals, not
a measurement of physical CPU residency. Wait intervals include checking an
already-satisfied dependency. Cost-model units are not nanoseconds. For automatic
selection, allow enough warmup to finish calibration before tracing (the default
is 256 ticks). A forced run may legitimately be slower.

Existing applications can capture the same structured `tracing` events by
enabling `celox::parallel=trace` and `celox::parallel::selection=debug` in their
subscriber. Detailed runtime buffers are allocated only when the trace target
is enabled. Events are emitted after all worker lanes finish; ordinary execution
does not format SIR or record timestamps. Use short capture windows because
per-expression and per-conflict compilation records can be large.

Viewer analysis tests: `node --test scripts/parallel-report.test.mjs`.

Use `--demo-stages 512` to increase work per worker when comparing synchronization
cost against useful computation. To refresh the viewer from an existing capture:

```sh
node scripts/render-parallel-report.mjs work/parallel-auto/report.json work/parallel-auto/report.html
```

For a Veryl project with an initial testbench, preserve project metadata and
source paths with `--project` (root, descendant directory, or `Veryl.toml`) and execute the original stimulus with
`--testbench-ticks LIMIT`:

```sh
cargo run --locked --release -p celox-bench --bin parallel-report -- \
  --project /path/to/heliodor --top test_soc_smp_linux_boot_4hart \
  --threads 4 --testbench-ticks 10000 --warmup 256 --capture 16 \
  --omit-sir --output work/heliodor-4core
```

In this mode, `--warmup` and `--capture` count outermost selected-kernel calls,
not ticks. Nested selections inside a captured composite call stay in the same
window until that outer call finishes.
A separate trace marker opens the window before the selected call starts, so
worker timestamps are disabled outside the capture window. Selector decisions
are retained throughout execution; decisions made during the capture window
may include perturbed timings. The original reset, memory images and testbench
processes run, but reaching the tick limit does not establish completed boot or
sequential equivalence. This mode does not report a speedup. Use repeated
`--source` arguments relative to the project root to restrict compilation to the
relevant source files. `--omit-sir` omits expanded SIR text while retaining
source expressions, placement candidates, physical footprints and runtime waits.
Each memory hazard includes one representative conflicting byte-range pair;
the footprints retain the full read/write effects. Source graphs display up to
300 nodes at a time; narrow the filter to inspect further nodes. The allocation
tables distinguish RTL instances from simulator lanes and count source paths,
not measured runtime work.
In testbench mode, `--observe SIGNAL` records the final value as a hexadecimal
snapshot; it does not compare it against another simulator. Constant `--set`
assignments are rejected because the testbench owns the input stimulus.

### Compare the plan with execution

Execution shows the recorded combinational partition estimate (left, abstract
cost units) beside the selected measured dispatch (right, nanoseconds). The
adopted candidate is selected initially when its recorded decision is available;
use Recorded static candidate to inspect alternatives. Its axis includes item
costs, task overhead and cross-lane synchronization, and its reconstructed
makespan must equal the recorded candidate score. It never uses measured task
durations or converts static weights to time.

The panels describe different compilation stages. Source partition tasks use
`pN` labels; optimized runtime tasks use `tN` labels. Equal numbers do not identify
the same task. Runtime dispatches can also contain FF work absent from the
combinational estimate. The measured phase is shown above the right panel. Click
either panel for its own metadata; the table below lists measured runtime tasks.
Changing the dispatch does not change the static estimate.

Schema-1 captures with Rust Debug candidate task tables are supported as well as
new structured tables. Missing or inconsistent static evidence produces an
explicit unavailable/error message, never a prediction derived from measurements.
Regenerate existing HTML using `scripts/render-parallel-report.mjs`.

The Task lineage table links adopted source tasks through their recorded
lowered unit indices to compiled tasks. Hover a `p` or `t` to highlight all
counterparts, including splits and merges; clicking shows the unit-level links.
Runtime rows distinguish FF-only tasks and tasks containing both comb and FF
units. These links describe compilation origin, not the fraction of instructions
that survived optimization or an allocation of measured time to source tasks.

For schema 1, this uses the compiler's preserved combinational unit order:
`lowered_task.units` indexes the comb vector, `compiled_task.units` indexes that
same vector for Comb, and Fused appends FF units after its comb prefix. The viewer
checks the adopted candidate's source items, lowering scope, complete unit
coverage, lanes and compiled Comb schedule before linking. Missing or ambiguous
evidence, or an unadopted candidate, displays an unavailable reason instead of
matching task numbers. This contract must be updated if a future optimizer
reorders or replaces the unit vector.

Click a timeline task or a task-table row to activate **Timeline focus**.
The original timelines highlight the selection and its compilation counterparts
in gold, predecessors in blue, and successors in purple, dimming unrelated tasks.
Each timeline zooms and scrolls to the selection; the full-timeline map marks the
visible range with a white box. Click the map to pan, choose a zoom level, or use
Locate selection to return to the task. Neighbor buttons follow predecessors or
successors; Back restores the previous selection.

Choose one to three hops, predecessors/successors/both, and whether to include
same-lane order. The optional dependency diagram shows completion-count waits as
solid edges and lane sequence as dashed edges, with no time axis. Lane order is
not necessarily direct data dependency. Full timeline restores the overview;
changing the dispatch or static candidate clears selection and navigation history.

To attribute long waits to their producers and physical memory conflicts:

```sh
node scripts/analyze-parallel-waits.mjs work/heliodor-4core/report.json
```

Open `wait-analysis.html` next to the report. It ranks dependency edges by
recorded blocking time, separates the time until producer completion from the
remaining notification/check delay, and links each edge to its reported
RAW/WAR/WAW byte-range witness and overlapping signal names. Filter by a signal
or task number and select a row for the original ranges and compiled units.
Missing or inconsistent producer records are reported instead of silently
attributed. Signal names include aliases and are approximate for strided array
storage; a physical ordering edge does not alone establish a semantic circuit
dependency. Summed waits across lanes are not wall-clock time or a promised
speedup from eliminating that edge.

Comb task formation compares unbounded merging with task budgets of 32 and 16
times the synchronization cost, charging the additional task and wait overhead
in each estimate. With zero synchronization cost (forced partitioning), the
budget interval has a floor of one, keeping the bounded candidates finite.
An indivisible item larger than the budget remains one task.
The candidate table's `Task budget` column and
`partition_decision.task_budget` expose this choice. After physical dependence
analysis, each consumer retains a publication boundary after its latest producer
on each other lane, so unrelated following work cannot delay notification.
Earlier producers only retain boundaries needed by another consumer. More tasks
and waits can still reduce execution time by releasing consumers sooner; verify the effect with
repeated untraced measurements.
