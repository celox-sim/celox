# VCD write activity and benchmarks

VCD recording has three costs: discovering changes, encoding changed values,
and writing the encoded bytes. The benchmarks report these separately from HDL
compilation and initialization. Both benchmarks use deterministic stimulus and
emit CSV; no timing threshold is a correctness test.

## Write activity

A VCD-enabled build reserves a `TraceLayout` between scheduler metadata and
backend scratch. Native x86/AArch64, Cranelift, Wasm, and the interpreter notify
this layout at Stable `Store` and `Commit` sites, alongside clock notifications.
A write to a Working object becomes visible when it commits. Native instruction
selection also covers combined packed stores and sparse commit worklists.

Each physical object's stable home selects a 64-byte group. A notification sets
one group byte and one summary byte covering 64 groups. Byte stores avoid an
extra read/modify/write in generated code. Native lowering coalesces repeated
notifications within a basic block. An observer skips unmarked summary groups,
consumes the marked group bytes, and compares only their associated signals.
The consumer reads each level eight bytes at a time. Empty words need one test;
nonempty words become a mask of nonzero byte positions, visited in address order.
It clears those words together and handles the remaining bytes separately.
Safe slice loads support unaligned metadata without reading past the last flag.
The mask exists only during collection; notification storage and generated
stores retain their byte layout.
Dense activity uses a direct scan. Aliases share the same home; a partial or
dynamically indexed write notifies observers of the complete object. These are
conservative write notifications: a notification does not imply a value change.

Clock notification bits are consumed during scheduling. Waveform activity is
consumed only at `dump`, so several commits or clock cascades between samples do
not lose updates. Tier promotion carries the activity in the existing state
allocation. The first dump always records every selected signal. Externally
supplied component values retain their existing ABI and are compared every dump.

Host setters use the same notification layout. Obtaining a mutable raw memory
view permanently selects full scanning for that memory image, since a retained
view can write at any later time without a setter notification. This includes
JavaScript shared-memory views and components that receive writable raw memory.
The buffered byte encoder still applies on these paths. Builds without VCD do
not reserve activity storage or generate notification stores.

The writer builds a trace plan at registration. Consecutive two-state one-bit
signals, two-state 64-bit signals, and generic signals form separate runs in
registration order. Fixed-width runs keep each memory offset, previous integer,
and precomputed suffix together; header names and scopes stay outside the hot
records. The initial snapshot has a separate loop specialization. Long fixed-width
runs dispatch once; their comparisons do not check per-signal type, initialization
flags, or previous-value offsets. The writer validates memory coverage once per
dump before unaligned fixed-width loads. A sparse dump with a shorter slice
validates only the selected ranges, and no selected signals means no memory access.
Sparse candidates use a one-signal specialization without changing their order
or alias IDs, avoiding a second traversal to rebuild runs from scattered indices.
When full-scan runs average fewer than two entries, the writer instead visits
registration entries with a one-signal specialization. This avoids setting up
a variable-length inner loop for every signal in an alternating-type trace.

Generic signals keep previous value and mask planes as packed bytes. Padding
above a signal's declared width is ignored. Four-state and external signals
compare both planes; two-state memory signals compare only values because their
previous mask stays zero for the writer's lifetime.
The one-bit path emits its ASCII digit directly. On x86-64 with SSE2 enabled,
the 64-bit path duplicates all input bytes once, then shuffles each required
pair into 16 output digits directly in spare capacity. Other targets retain
the scalar lookup-table encoder. It publishes only the significant digits,
preserving leading-zero abbreviation and the single digit for zero.
Other widths and four-state/external values use the generic byte
encoder; unknown bits retain VCD's `x`/`z` distinction. A changed vector
emits its complete VCD value, even if only one bit changed.
Header construction precomputes each record's suffix: a space for vectors,
the ID, and a newline. Suffixes of up to eight bytes are stored inline and
copied with a fixed-size store; longer IDs retain a separate byte allocation.
The encoders write directly into checked slices of the output block's spare
capacity. The writer publishes the complete record with one Vec length update.
Padding written by SIMD or the fixed-size suffix copy stays outside that length.
Blocks at least as large as the `BufWriter` capacity
(256 KiB) bypass its internal copy; a dump's final partial block is handed to
`BufWriter` before returning. Headers, timestamps, and value records therefore
retain their order, including when a single record exceeds the block size.
The block reserves capacity for 256 KiB plus the largest possible record,
including fixed-store padding, at the first dump. No record grows the Vec;
the allocation adds roughly 256 KiB compared with a single-record buffer.
Header construction stays in a separate cold function. The pending change
count belongs to the output block. Comparison statistics update once per run;
an I/O error records exactly the visited prefix, including unchanged values.
The initial snapshot is complete only after its value records are handed to
`BufWriter`; a failed initial dump retries a full snapshot on the next dump.

`VcdWriter::flush`, `Simulator::flush_vcd`, or `Simulation::flush_vcd` explicitly
publishes pending bytes and reports I/O errors. Dropping the writer flushes
through `BufWriter`, but cannot report errors. JavaScript handles report flush
failures from `dispose()`. These flushes do not request durable-storage `fsync`.

VCD currently describes whole objects as packed memory. Factories compiling
source with VCD therefore select the packed layout, including native image
compilation. Imported images retain their layout: an uninstrumented image uses
full scanning, and incompatible element-strided arrays produce a construction
error requesting recompilation with VCD enabled.
The waveform metadata changes the native image container version from 4 to 5;
old containers must be regenerated.

## Writer benchmark

```sh
VCD_MODE=scan cargo bench --locked -p celox-runtime --bench vcd
VCD_MODE=dirty cargo bench --locked -p celox-runtime --bench vcd
VCD_MODE=collect cargo bench --locked -p celox-runtime --bench vcd
```

`scan` compares every signal. `dirty` measures the production notifications,
consumption, candidate selection, comparison, and encoder. `collect` measures
mutation and activity collection without encoding. `VCD_OUTPUT=count` (default)
consumes the actual encoded buffer and counts bytes without system calls;
`VCD_OUTPUT=/dev/null` includes file writes but excludes storage latency. Set a
local file path to include filesystem output. Each sample recreates the file.

| Variable | Default | Meaning |
| --- | --- | --- |
| `VCD_SIGNALS` | 16384 | Independently declared signals |
| `VCD_WIDTH` | 64 | Width per signal |
| `VCD_FOUR_STATE` | 0 | Use value and mask planes when set to 1 |
| `VCD_STEPS` | 1000 | Dumps after the initial snapshot |
| `VCD_REPEATS` | 3 | Independent repetitions |
| `VCD_CASE` | all | Run one case from the table below |

| Case | Stimulus / purpose |
| --- | --- |
| `idle` | No writes after initialization; measures dependence on total state size |
| `sparse_clustered` | Approximately 0.1% of signals updated in adjacent homes |
| `sparse_scattered` | Same update count spread across homes, rotating each step |
| `same_value` | Scattered writes of unchanged values; notification overhead with no output |
| `dense` | Every signal changes; encoding throughput and dense fallback |
| `burst` | Every signal changes once per 100 steps; activity bursts |
| `mask_only` | Only mask bits change; enabled in four-state runs |

Useful sweeps are 1024/16384/131072 signals and widths 1/9/64/65/256/1024.
Clustered versus scattered updates quantify the extra comparisons caused by
coarse groups. Initialization uses nonzero values and is followed by an explicit
flush before timing. The timed region includes mutation and the final flush.
Mutation runs in a separate, non-inlined `Stimulus::apply` function. This keeps
collector changes from also changing register allocation in the measurement's
per-signal input loop. Case and notification settings are prepared before timing;
the actual input writes and notifications remain timed. Rebuild both library
versions with this same harness when comparing them.
CSV reports wall time, comparisons, changed signals, and actual emitted bytes;
`scan` and `dirty` must have identical change counts and output byte counts.

Keep both `collect / idle` and `collect / same_value` as controls when changing
the writer or collector. They expose collection costs without timed encoding;
`dirty / idle` also includes timestamp output. Use millions of idle dumps and
include small layouts with fewer than eight summary bytes, alongside larger
layouts. Run timing executables serially, without overlapping builds or tests,
and retain retired instructions as well as elapsed-time ranges: code placement
can change elapsed time even when the executed work is unchanged.

## End-to-end benchmark

```sh
cargo bench --locked -p celox --bench vcd
VCD_BACKEND=cranelift cargo bench --locked -p celox --bench vcd
```

This benchmark compiles independent counters with individual enables and
increments, avoiding accidental merging of identical counters. It samples both
clock phases and explicitly settles combinational logic even with tracing off.
Idle, approximately 1% enabled, and all-enabled workloads run in four modes:

- `off`: VCD disabled, including write instrumentation.
- `instrumented`: notifications enabled, no dumping; isolates simulation overhead.
- `scan`: notifications enabled but full scanning forced through the raw-view fallback.
- `dirty`: notifications and incremental dumping enabled.

Defaults are `VCD_SIGNALS=256`, `VCD_STEPS=10000` full cycles,
`VCD_REPEATS=3`, `VCD_BACKEND=native`, and `VCD_OUTPUT=/dev/null`.
`VCD_MODE` and `VCD_CASE` select a single mode or workload; `interpreter` is also
an available backend. `VCD_FULL_WIDTH=1` sets bit 63 in the reset values so all
64 bits remain significant during the measured runs; the default is zero.
Here `VCD_SIGNALS` counts counters; the default fixture
records 514 signals including enables, clock, and reset. The idle case still
toggles the clock. Construction, reset, initial configuration, and the initial
waveform snapshot are excluded. Final flush is included. The scalar fixture has
the same stable data layout in all modes. CSV value bytes exclude timestamps;
the writer benchmark reports total bytes including timestamps.

Measure compilation separately. Run timing samples without concurrent builds or
tests, retain individual samples and compare medians, and record the revision,
CPU, Rust version, backend, and output destination. For filesystem measurements,
record the filesystem and use a sufficiently long run to expose sustained
throughput. `/usr/bin/time -v` can separately record peak RSS.

Heliodor is a useful subsequent macro workload, but its existing benchmark
runner does not sample VCD. A comparable tracing run needs explicit matching
signal sets, time units, reset/unknown-state semantics, and sampling points in
both simulators. Prefer fixed idle/compute/memory-heavy windows to dumping an
entire Linux boot. No Heliodor tracing performance claim follows from the
microbenchmarks here.

## Measured results

These initial measurements predate output-block encoding.

Measured on 2026-09-10 with the changes on `perf/vcd-change-tracking`, based on
`f114181392ef23ed8fc2c4fb102f4041821999f7`. The host reports an AMD Ryzen 7 9800X3D
under virtualized Linux; processes were pinned to CPU 0. Rust was 1.98.1, using
the optimized bench profile with LTO. No builds or tests ran concurrently.
The following are elapsed-time medians in milliseconds, writing to `/dev/null`.
Both `scan` and `dirty` use the new byte encoder and buffering, so these ratios
measure incremental observation against the full-scan control in this change.

The writer used 16,384 64-bit two-state signals, 1,000 dumps, and five samples:

| Case | Full scan (ms) | Incremental (ms) | Scan / incremental |
| --- | ---: | ---: | ---: |
| `idle` | 86.609 | 0.035 | 2507.78× |
| `sparse_clustered` | 88.668 | 0.493 | 180.02× |
| `sparse_scattered` | 90.366 | 1.926 | 46.92× |
| `same_value` | 88.413 | 1.573 | 56.22× |
| `dense` | 359.811 | 418.889 | 0.86× |
| `burst` | 95.709 | 3.793 | 25.23× |

Scattered sparse updates reduced comparisons from 16,384,000 to 127,988 while
emitting the same 16,000 changes and 1,116,063 bytes. A completely idle writer
performed no value comparisons; it still scanned the small summary and emitted
timestamps. Fully dense updates were 16% slower in this measurement: notification
and collection costs remain even when the writer chooses its dense scan. Dense
samples varied from 359–418 ms for `scan` and 368–500 ms for `dirty`, so the
precise regression needs more samples before treating it as stable.

The compiled counter workload used 256 counters and 10,000 full cycles (20,000
dumps), with five native samples and three Cranelift samples:

| Backend | Case | Off (ms) | Instrumented (ms) | Full scan (ms) | Incremental (ms) | Scan / incremental |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| native | idle | 1.203 | 1.661 | 61.479 | 7.937 | 7.75× |
| native | sparse | 1.262 | 1.799 | 60.563 | 9.317 | 6.50× |
| native | dense | 1.439 | 2.111 | 97.848 | 73.100 | 1.34× |
| Cranelift | idle | 1.006 | 1.759 | 59.625 | 33.659 | 1.77× |
| Cranelift | sparse | 1.055 | 1.759 | 60.370 | 42.723 | 1.41× |
| Cranelift | dense | 1.057 | 1.805 | 97.725 | 73.317 | 1.33× |

Instrumentation has a measurable simulation cost before dumping: approximately
0.46–0.67 ms per 10,000 cycles in the native fixture. Native sparse comparisons
fell from 10,280,000 to 760,000. Cranelift sparse comparisons fell to 5,500,000;
conservative notifications reflect executed stable writes, so unchanged values
can still require comparison. Its sparse incremental samples ranged from
35.992–43.867 ms. Dense end-to-end gains include skipping most comparisons on
the low clock phase, unlike the writer's every-signal-every-dump dense case.

Three-sample sweeps also covered 1,024 and 131,072 signals, giving 38.43× and
43.57× for scattered sparse updates. Four-state mask-only updates at widths
65/256/1024 took 2.071/4.827/19.038 ms incrementally, versus
86.731/90.480/142.172 ms for full scans. All scan/incremental pairs had identical
change and output-byte counts. Local ext4 file output for scattered updates
also matched byte-for-byte after excluding the date header; that short run does
not measure sustained storage throughput or durable writes.

Individual CSV samples and the run manifest are saved locally under
`target/vcd-benchmark-results/` (ignored build artifacts). Reproduce the main
writer measurements with `VCD_OUTPUT=/dev/null VCD_REPEATS=5` and each of
`VCD_MODE=scan`, `dirty`, and `collect`; use `VCD_REPEATS=5` for the native
end-to-end benchmark and `VCD_BACKEND=cranelift VCD_REPEATS=3` for Cranelift.
On Linux, prefix the command with `taskset -c <cpu>` to pin to an available CPU.

## Hardware profiling on WSL

This profile also predates output-block encoding.

On the measured host, `/usr/bin/perf` was an Ubuntu launcher that could not find
a binary under the running WSL kernel's version name. The already installed
`/usr/lib/linux-tools-6.8.0-139/perf` (perf 6.8.12) successfully read hardware
counters on kernel `6.18.33.2-microsoft-standard-WSL2`. A user-local symlink now
makes that binary available through the normal `perf` command:

```text
~/.local/bin/perf -> /usr/lib/linux-tools-6.8.0-139/perf
```

No additional build or kernel setting change was needed. Check the actual
binary and hardware access before collecting profiles:

```sh
command -v perf
perf version
perf stat -e cycles:u,instructions:u,branches:u,branch-misses:u -- true
```

The `:u` events count user-mode execution. Profile the compiled benchmark
executable directly to exclude Cargo and compilation from the counters.

A follow-up on 2026-09-10 used the same writer binary, CPU 0, `/dev/null`,
16,384 64-bit two-state signals, and 1,000 dense steps. After one warm-up per
mode, seven independent processes per mode ran in alternating order. All four
events reported 100% running time, without multiplexing. These counters cover
the entire process, including fixture construction and the initial snapshot;
the benchmark's own elapsed time still excludes those phases.

| Median | Full scan | Incremental |
| --- | ---: | ---: |
| Cycles | 3,024,609,647 | 3,033,034,066 |
| Instructions | 9,216,358,029 | 9,348,346,641 |
| Branch misses / branches | 0.0325% | 0.0340% |

The incremental path executed 1.43% more instructions, with a 0.28% difference
in median cycles. Wall time varied substantially: 785–1,096 ms for full scans
and 894–1,683 ms for incremental dumps. The earlier 16% wall-time gap therefore
does not establish a stable 16% execution-cost increase. Branch misprediction
does not appear to dominate this dense workload. This measurement does not
predict the cost of adding a new comparison or branch at every generated store.

Cycle sampling also worked: `perf record -F 499 -e cycles:u --call-graph dwarf`
on 5,000 incremental dense steps captured 2,053 samples with no reported loss.
The largest self-time shares were `VcdWriter::dump_with_activity` (48.4%) and
libc's `memmove` (38.5%), followed by the benchmark's `main` (7.7%) and
`memcmp` (3.3%). The binary retains symbols but strips source debug
information, so these results identify functions rather than source lines.

Raw counter files, benchmark CSVs, a summary, the executable hash, and the
sampling recording are in `target/vcd-perf-results/` (ignored build artifacts).

## Output blocks and two-state comparison

A subsequent comparison on 2026-09-10 measures output blocks and the two-state
mask invariant described above. Both binaries already include activity tracking
and the byte encoder; these are additional gains over the preceding
implementation. The host, compiler, bench profile, and CPU affinity are the same
as above. Five independent processes per binary ran in alternating before/after
order, writing to `/dev/null`, without concurrent builds or tests. Timings below
are medians; construction and the initial snapshot are excluded, and the final
flush is included.

The writer used 16,384 64-bit two-state signals and 1,000 dumps:

| Mode | Case | Before (ms) | After (ms) | Time reduction |
| --- | --- | ---: | ---: | ---: |
| `dirty` | idle | 0.0343 | 0.0339 | approximately unchanged |
| `dirty` | sparse_clustered | 0.481 | 0.371 | 22.8% |
| `dirty` | sparse_scattered | 1.711 | 1.299 | 24.1% |
| `dirty` | same_value | 1.490 | 1.029 | 31.0% |
| `dirty` | dense | 415.414 | 305.684 | 26.4% |
| `dirty` | burst | 3.925 | 2.892 | 26.3% |
| `scan` | idle | 109.634 | 73.445 | 33.0% |
| `scan` | dense | 418.216 | 309.759 | 25.9% |

Longer sparse runs of 100,000 dumps also improved: clustered updates took
48.856 to 42.377 ms (13.3% reduction), and scattered updates took 177.567 to
134.241 ms (24.4%). The unchanged mask check is skipped only for two-state
memory signals. External signals still compare masks when the current value
is known, preserving transitions from `x` or `z` to zero.

The native counter workload used 256 counters (514 traced signals) and 100,000
full cycles, or 200,000 dumps:

| Mode | Case | Before (ms) | After (ms) | Time reduction |
| --- | --- | ---: | ---: | ---: |
| `scan` | idle | 685.748 | 530.723 | 22.6% |
| `scan` | sparse | 704.517 | 540.259 | 23.3% |
| `scan` | dense | 1125.349 | 888.444 | 21.1% |
| `dirty` | idle | 81.032 | 69.654 | 14.0% |
| `dirty` | sparse | 112.928 | 85.270 | 24.5% |
| `dirty` | dense | 892.650 | 641.617 | 28.1% |

Hardware counters corroborate the dense writer gain. For the incremental
1,000-dump workload, median cycles fell from 2,051,929,560 to 1,509,797,721
(26.4%), and instructions from 9,348,339,330 to 7,214,076,419 (22.8%). These
counters cover the whole process, including initialization, and all events
reported 100% running time without multiplexing. A separate pair of cycle
profiles at 499 Hz over 5,000 dense dumps captured 1,032 and 772 samples with
no reported loss. Libc `memmove` self-time accounted for 56.2% before and 16.2%
after. Sampling shares alone do not measure absolute time, but agree with the
removal of the per-record copy into `BufWriter`.

Four-state mask-only workloads remain sensitive to width. At 65 bits the
median changed from 2.049 to 1.875 ms. At 1,024 bits it changed from 18.505 to
18.970 ms; the ranges overlap (17.209–22.985 versus 18.726–23.918 ms), so this
run establishes no improvement for that case. The output block adds roughly
256 KiB per writer. These `/dev/null` measurements do not establish sustained
filesystem throughput or gains for other simulation workloads.

Every before/after pair had identical comparison, change, and output-byte
counts. A separate dense file comparison matched byte-for-byte after excluding
the date header. Saved binaries and source snapshots, SHA-256 hashes, individual
CSV and counter samples, the comparison driver, and profiles are under
`target/vcd-block-results/` (ignored build artifacts). The manifest records each
command and environment. The existing benchmark commands reproduce the
workloads; use `VCD_STEPS=100000` for the longer sparse and native runs.

## Verilator comparison

```sh
python3 scripts/compare-vcd-verilator.py
```

This Linux runner builds the Celox native benchmark and equivalent SystemVerilog
models, then checks their VCD output against an independent counter oracle before
timing. It requires Verilator, GCC, Make, Python 3, and Cargo. Use `--verilator`
for a Verilator executable outside `PATH`, `--celox` for an already built Celox
benchmark, and `--output` for a separate results directory. Defaults are 256
counters, 100,000 full cycles, five independent processes per combination, and
the first available CPU. `--signals`, `--steps`, `--repeats`, and `--cpu` override
those settings. `--check-only` builds and validates without collecting timings.
`--reuse-builds` reruns validation and measurements using binaries checked
against the preceding manifest, without recompiling the C++ models. Pass
`--celox` with this option to replace the cached Celox executable. The runner
sets a 64 MiB process and Rust worker stack (`--stack-mib`); the 4,096-counter
fixture overflowed a frontend worker's default stack during construction.
`--baseline-celox PATH` adds an earlier Celox executable to the same validation
and rotating measurement order, avoiding comparisons made only across separate
runs. Its input is snapshotted before replacing cached executables, so it may
refer to the previous `celox-vcd` in the output directory. `--cases dense` and
`--modes vcd` select a focused timing run; waveform validation still runs for
every selected case and engine.

Both engines run idle, approximately 1% enabled, and fully enabled cases, with
VCD disabled, instrumentation only, and VCD output enabled. Verilator uses
separate traced and untraced builds; the traced binary also supplies the
instrumentation-only control. Celox uses its incremental native path. Both
timers exclude construction, reset, one enabled setup tick, and the initial
snapshot, and include both clock phases and the final flush. Samples alternate
engine order, use one simulation thread pinned to one CPU, and write to
`/dev/null`. Compilation finishes before the timing runs. Rust uses the bench
profile with LTO; generated C++ and its runtime use GCC `-O3 -flto`, with
`OPT_FAST`, `OPT_SLOW`, and `OPT_GLOBAL` all set to `-O3`.

The verifier checks every signal's width and transitions over 20 cycles per
combination (`--check-steps`), including initial values,
active-low reset semantics, and nanosecond timestamps. It accepts differences
in VCD identifiers, scopes, leading zeros, and empty timestamps, while rejecting
missing signals or differing values. Verilator's extra top wrapper trace and
parameter tracing are disabled to retain the same signal set. The C++ harness
also checks every final counter outside the timed region, including in the
untraced build.

Two value patterns expose an encoding difference. `counter` uses the original
small reset values. Celox abbreviates leading zero bits, whereas Verilator emits
all 64 bits. `full_width` sets bit 63 so both writers emit all 64 value bits;
identifier lengths can still differ. Verilator's sink counts actual writes;
Celox's total adds timestamp lengths to its value-byte statistics. Headers and
initial snapshots are excluded from these byte counts. Files, individual CSVs,
commands, versions, executable hashes, and medians/ranges are retained in
`target/vcd-verilator-results/` by default.

### What Verilator generates

The inspected version is Verilator 5.052 (tag `v5.052`, commit
`ea338be98e1e838d3518809ce8899f85a009963c`). Its compiler groups signals by
activity, inserts byte flags, and can choose unconditional comparison when the
estimated comparisons are cheaper than testing the flags. This supports the
same general design choice of conservative write activity followed by value
comparison. Its groups follow generated execution dependencies, rather than
Celox's physical memory homes.
[Source: V3Trace.cpp](https://github.com/verilator/verilator/blob/v5.052/src/V3Trace.cpp).

Generated trace calls specialize comparison by width: `chgBit`, `chgIData`, and
`chgQData` compare native integers and emit only on a difference. The VCD writer
encodes directly into its output buffer, keeps precomputed identifier/newline
suffixes, and uses SIMD binary-to-ASCII conversion on x86. Its single-threaded
path avoids a separate record buffer. These are useful candidates for further
Celox optimization if profiles show comparison dispatch or encoding dominating.
[Comparison source](https://github.com/verilator/verilator/blob/v5.052/include/verilated_trace.h),
[encoding source](https://github.com/verilator/verilator/blob/v5.052/include/verilated_trace_imp.h),
[output source](https://github.com/verilator/verilator/blob/v5.052/include/verilated_vcd_c.cpp).

The counter fixture exposes every counter as a top-level output. In the inspected
generated code, all these ports use unconditional change checks after a global
activity guard. This fixture therefore tests a specialized comparison loop and
encoding throughput; it does not establish how either engine's activity
selection behaves on a large hierarchy with mostly quiet internal state.

### Baseline measured against 5.052

These measurements precede the integer and SIMD specialization below. They use
the same Ryzen 7 9800X3D host under WSL, CPU 0, Rust 1.98.1,
and GCC 13.3.0. Verilator was built from the release tag above in
`target/verilator-v5.052/`; no system installation was needed. The standard
Ubuntu package available on this host was 5.020 and was not used for this
comparison. An additional Flex header needed for the source build was extracted
from Ubuntu's `libfl-dev` package into `target/`.

The commands below reproduce the two sizes after the local Verilator build:

```sh
python3 scripts/compare-vcd-verilator.py \
  --verilator target/verilator-v5.052/bin/verilator
python3 scripts/compare-vcd-verilator.py \
  --verilator target/verilator-v5.052/bin/verilator \
  --celox target/vcd-verilator-results/celox-vcd \
  --output target/vcd-verilator-4096 \
  --signals 4096 --steps 10000 --patterns full_width
```

For 256 counters (514 traced signals), 100,000 cycles, and the `full_width`
pattern, median elapsed times in milliseconds were:

| Case | Celox off | Verilator off | Celox instrumented | Verilator instrumented | Celox VCD | Verilator VCD |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| idle | 12.471 | 16.838 | 17.532 | 17.130 | 66.844 | 56.008 |
| sparse | 12.410 | 16.866 | 17.177 | 16.523 | 81.114 | 62.141 |
| dense | 15.306 | 18.024 | 22.075 | 17.911 | 637.877 | 146.975 |

For 4,096 counters (8,194 traced signals), 10,000 cycles, and the same
`full_width` pattern:

| Case | Celox off | Verilator off | Celox instrumented | Verilator instrumented | Celox VCD | Verilator VCD |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| idle | 17.206 | 70.308 | 20.219 | 71.370 | 25.918 | 112.966 |
| sparse | 17.029 | 69.355 | 19.809 | 82.345 | 38.855 | 115.628 |
| dense | 22.115 | 63.130 | 33.676 | 63.986 | 942.670 | 294.993 |

Thus Verilator was 4.34 times as fast in the smaller dense case and 3.20 times
as fast in the larger dense case. Celox was 4.36 times as fast in the larger
idle case and 2.98 times as fast in its sparse case. These ratios
include simulation; the off and instrumented controls are necessary to assess
the tracing contribution. For example, in the larger idle case, the difference
between VCD and instrumented medians was about 5.7 ms for Celox and 41.6 ms for
Verilator. Subtracting medians is an indication of additional cost, not a
separately measured writer-only time.

Output volume does not explain the dense gap. In the smaller `full_width`
case Celox emitted 1,768,488,897 timed bytes and Verilator 1,765,488,894; in the
larger case they emitted 2,826,428,895 and 2,837,328,892. For the smaller original
`counter` pattern, Celox emitted fewer bytes (724,756,054 versus 1,765,488,894)
but still took 703.325 ms versus 155.987 ms.

Each table entry is the median of five processes. The smaller dense VCD
`full_width` samples ranged from 626.880–669.425 ms for Celox and 144.569–186.808
ms for Verilator; the larger dense samples ranged from 887.611–1,048.264 ms and
287.609–321.733 ms. Some shorter controls varied much more under virtualization.
All samples are retained. The first 256-counter run, before adopting the larger
stack and cached-build runner options, remains in
`target/vcd-verilator-results/initial-run/`; the tables use the subsequent full
repeat under the final runner settings.

A separate profile used 256 counters, the `full_width` dense pattern, and
1,000,000 cycles at 499 Hz. Celox collected 3,443 cycle samples with 53.6% self
time in `dump_with_activity`, 21.7% in libc `memcmp`, 12.6% in `memmove`, and
3.6% in `memset`. The `memcmp` call chain came almost entirely from the VCD
writer. Verilator collected 758 samples, with 62.8% in its specialized
`fullQData` routine and 11.5% in the generated change-check function. Neither
profile reported lost samples. These profiles cover the whole process, including
construction, and sample shares are not absolute time comparisons.

The measurements motivated retaining sparse activity selection while
specializing integer comparisons, previous-value copies, and SIMD encoding
directly into the output block, as measured below. Neither
this top-level port fixture nor `/dev/null` establishes results for four-state
simulation, storage throughput, or a mostly quiet internal hierarchy such as
Heliodor. A hierarchy benchmark is still needed to compare dependency-based
activity groups with physical-home groups on that workload.

### Integer comparison and SIMD encoding

The subsequent implementation specializes two-state memory signals of widths
1 and 64. Registration selects the comparison path once; the 64-bit path uses
unaligned-safe fixed-size loads/copies and SSE2 encoding into the output block.
The generic cases share a match arm, avoiding a chain of separate integer-kind
tests before their normal processing. There is no PGO or generated-code
rewriting, and SSE2 selection is at compile time. AArch64 retains scalar
encoding and was compile-checked; SIMD performance was measured only on x86-64.

The writer comparison uses 16,384 signals, 1,000 dumps, CPU 0, `/dev/null`, one
warm process and five measured processes per binary/case. Binary order rotates
between samples. Construction and the first dump are outside the timer;
stimulus, activity collection, encoding, and final flush are inside it. Every
pair has identical comparison counts, change counts, and emitted byte counts.
The baseline is the output-block writer used in the preceding Verilator tables.
Median times in milliseconds:

| Width / states | Mode | Case | Before | Integer + SIMD |
| --- | --- | --- | ---: | ---: |
| 64 / 2 | scan | idle | 65.241 | 23.011 |
| 64 / 2 | scan | dense | 272.424 | 129.963 |
| 64 / 2 | dirty | idle | 0.033 | 0.033 |
| 64 / 2 | dirty | sparse scattered | 1.241 | 0.796 |
| 64 / 2 | dirty | same-value writes | 1.009 | 0.663 |
| 64 / 2 | dirty | dense | 276.351 | 132.233 |
| 1 / 2 | dirty | dense | 190.196 | 93.093 |

An intermediate three-way run separated the two optimizations: the 64-bit dense
scan took 274.368 ms before, 211.782 ms with only integer comparison/copy, and
129.732 ms after adding SSE2. These are separate samples from the final table,
not additive estimates. The one-bit specialization was added after measuring
a regression in one-bit workloads with only the 64-bit path enabled.

The generic path is not uniformly faster. The final five-process series measured
the following controls, also using 16,384 signals and 1,000 dumps:

| Width / states | Dirty case | Before | Integer + SIMD |
| --- | --- | ---: | ---: |
| 32 / 2 | dense | 233.574 | 239.979 |
| 63 / 2 | dense | 275.576 | 280.016 |
| 65 / 2 | dense | 292.896 | 293.939 |
| 256 / 2 | dense | 455.789 | 457.183 |
| 64 / 4 | dense | 319.290 | 345.745 |
| 64 / 4 | mask-only sparse | 2.043 | 2.253 |

The two-state generic widths differed by 0.3–2.7%, and the four-state cases
were 8–10% slower in that series. Short controls varied between runs; for
example, a separate seven-process series with 5,000 mask-only dumps measured
9.966 versus 10.164 ms. These results support the two-state integer optimization,
not a four-state speedup or a general no-regression claim. Individual CSVs,
intermediate binaries, source snapshots, and hashes are retained under
`target/vcd-integer-results/`, with final writer measurements in
`writer-final-simd-summary.json` and `writer-final-fallback-summary.json`.

### Native results after integer specialization

The final comparison also runs the saved pre-specialization Celox executable
alongside the new executable and Verilator 5.052. It rotates all three engines
within each repetition and validates all three waveforms against the same
counter oracle. Both Celox versions emit exactly the same number of timed
bytes. The `full_width` dense results below use five processes per engine,
CPU 0, `/dev/null`, and the preceding compilation/initialization exclusions:

| Counters | Full cycles | Celox before (ms) | Celox after (ms) | Verilator (ms) | Celox speedup |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 256 | 100,000 | 608.066 | 255.871 | 147.186 | 2.38x |
| 4,096 | 10,000 | 933.196 | 388.599 | 292.175 | 2.40x |

Celox's dense elapsed time fell by about 58%. Verilator remains 1.74 times as
fast in the smaller case and 1.33 times as fast in the larger one. These are
whole-simulation comparisons, including tracing. The 256-counter ranges were
602.127–661.866 ms before, 249.008–284.738 ms after, and 140.793–152.421 ms for
Verilator. The 4,096-counter ranges were 913.549–1,018.107 ms,
368.478–434.953 ms, and 286.747–328.943 ms respectively.

The full control run for 4,096 counters also retained the advantage on sparse
activity. These are separate samples from the focused dense comparison above:

| Case | Celox off | Verilator off | Celox instrumented | Verilator instrumented | Celox VCD | Verilator VCD |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| idle | 16.462 | 67.116 | 18.912 | 66.575 | 21.945 | 116.436 |
| sparse | 16.576 | 65.826 | 18.636 | 69.502 | 30.188 | 118.378 |
| dense | 20.432 | 64.008 | 31.612 | 64.943 | 373.489 | 320.014 |

The host varied substantially during the earlier 256-counter runs: both
engines' off/instrumented controls roughly doubled during one interval. The
first three-way dense run had a baseline range of 1,367–2,392 ms and a new
Celox range of 569–836 ms. That run is retained in `paired-256/`; the table uses
one subsequent run restricted to dense `full_width`, retained in
`paired-256-dense/`. The larger focused run is in `paired-4096/`, and the full
control runs are in `verilator-256/` and `verilator-4096/`, all beneath
`target/vcd-integer-results/`. Timing variability limits conclusions about
small differences and the winner in the smaller sparse case. No samples were
removed from a reported median.

For the verified cached fixtures, the focused comparison can be repeated with:

```sh
python3 scripts/compare-vcd-verilator.py \
  --verilator target/verilator-v5.052/bin/verilator \
  --celox target/vcd-integer-results/e2e-final \
  --baseline-celox target/vcd-integer-results/e2e-before \
  --output target/vcd-integer-results/paired-256-dense \
  --patterns full_width --cases dense --modes vcd --reuse-builds
```

Use `--signals 4096 --steps 10000` and `paired-4096` for the larger fixture.
Omit `--reuse-builds` when populating a fresh output directory. The comparison
script itself tests baseline caching when the input path is the executable
being replaced, preventing an accidental comparison of two copies of the new
binary.

A final profile repeated the 256-counter `full_width` dense workload for
1,000,000 cycles at 499 Hz. It collected 1,584 samples with none lost. `memcmp`
fell from 21.7% of cycle samples in the earlier profile to 0.08%; the final
profile had 69.3% self time in `dump_with_activity` and 11.9% in `memmove`.
About 11.1 percentage points of the latter had the VCD writer as caller.
This is consistent with removing generic comparison and previous-value copying
for integer signals while retaining encoding and output copies. These profiles
include construction, and sample shares are not absolute time measurements.
The data, command, environment, and binary hash are retained as
`profile-final.*` and `profile-manifest.json` in the same results directory.

### Complete-record output

The next change uses commit `4c9a50735` (integer comparison and SSE2 encoding)
as its baseline. It precomputes the space/ID/newline suffix, replaces short-ID
copies with a fixed eight-byte store, and updates the output Vec length once
per complete record. The encoding algorithm, significant-digit abbreviation,
activity selection, and output-block/flush policy are retained. Checked spare
capacity includes suffix padding even for scalar-only traces. Long suffixes
use a separate byte slice. No generated-code rewriting, PGO, or additional
instruction-set requirement is introduced.

The writer comparison again uses 16,384 signals, 1,000 dumps, CPU 0,
`/dev/null`, one warm process and five measured processes per binary/case,
with alternating binary order. Comparison counts, change counts, and emitted
byte counts match in every pair. Median times in milliseconds:

| Width / states | Mode | Case | Before | Complete records |
| --- | --- | --- | ---: | ---: |
| 64 / 2 | scan | dense | 139.909 | 99.210 |
| 64 / 2 | dirty | dense | 146.960 | 116.869 |
| 1 / 2 | dirty | dense | 99.294 | 69.621 |
| 32 / 2 | dirty | dense | 272.081 | 244.892 |
| 63 / 2 | dirty | dense | 305.122 | 264.444 |
| 65 / 2 | dirty | dense | 314.191 | 292.528 |
| 256 / 2 | dirty | dense | 508.588 | 456.981 |
| 64 / 4 | dirty | dense | 347.422 | 345.496 |

An alternating three-process `perf stat` control on the 64-bit dirty dense
case measured 4.149 billion instructions before versus 3.393 billion after,
including process setup, an 18.2% reduction. The short-ID copy in the compiled
writer is an inline fixed-size store. The SIMD algorithm remains unchanged.

Short sparse samples were noisy, so a separate control increased their length
to 100,000 dumps. This reproduced a regression in the standalone benchmark:

| Width / states | Dirty case | Before (ms) | Complete records (ms) |
| --- | --- | ---: | ---: |
| 64 / 2 | idle | 3.907 | 3.794 |
| 64 / 2 | sparse scattered | 91.127 | 100.434 |
| 64 / 2 | same-value writes | 73.319 | 84.626 |
| 64 / 4 | mask-only sparse | 220.271 | 239.759 |

These regressions are retained rather than discarded. In three alternating
hardware-counter samples, same-value instructions remained almost identical
(2.170 versus 2.166 billion), while cycles increased. Profiles of 2,000,000
same-value dumps locate most additional cycle samples in the benchmark main
function's activity-collection loop; they do not establish a per-record writer
regression. Both profiles reported no lost samples. A subsequent five-process
`collect` control, which never calls the writer during the timed loop, also
reproduced the difference: same-value writes took 32.527 versus 45.823 ms;
sparse scattered writes took 32.565 versus 52.950 ms; mask-only writes took
37.130 versus 47.515 ms. This establishes an effect outside the timed writer
calls. The code-placement experiments below investigate its cause. Do not
subtract these separate samples to estimate isolated writer cost. Use the
native controls below to assess the effect on simulation.

Raw samples, binary hashes and source snapshots are under
`target/vcd-record-results/`. The writer tables are reproduced by
`compare-writer.py`, `compare-writer.py --fallback`, `compare-controls.py`, and
`compare-collection.py`
in that directory. Their summaries retain all five samples per binary/case;
no sample was removed from a reported median.

### Native results after complete-record output

The native comparison uses saved executables for `4c9a50735`, the record-output
change, and the same Verilator 5.052 fixtures. It rotates engine order and runs
five measured processes after a warm process per engine/mode. CPU 0,
`/dev/null`, and the preceding construction/initial-snapshot exclusions apply.
All three engines pass the 20-cycle waveform oracle. Old and new Celox also
emit the same timed byte counts, and their validation waveform bodies match
byte for byte after the header. At 256 counters and 100,000 full cycles,
median elapsed times with VCD enabled are:

| Pattern | Case | Celox before (ms) | Complete records (ms) | Verilator (ms) |
| --- | --- | ---: | ---: | ---: |
| counter | idle | 46.728 | 38.650 | 58.263 |
| counter | sparse | 48.215 | 48.598 | 56.958 |
| counter | dense | 250.238 | 186.323 | 146.935 |
| full_width | idle | 38.162 | 38.495 | 57.582 |
| full_width | sparse | 47.965 | 49.459 | 58.575 |
| full_width | dense | 284.127 | 211.982 | 142.041 |

Dense elapsed time fell by about 25% in both patterns. The `full_width` gap
relative to Verilator narrowed from 2.00x to 1.49x in this paired run. Dense
ranges were 253.461–311.118 ms before, 204.028–217.089 ms after, and
140.161–157.244 ms for Verilator. Native sparse elapsed times differed by
0.8–3.1%; individual ranges overlap. These results do not support a sparse
speedup from record encoding.

The no-output controls are also retained. For `full_width` dense, off took
14.495 versus 14.656 ms, while instrumented-without-dumping took 21.034 versus
28.829 ms. The latter regression also appeared in `counter` dense
(21.685 versus 29.257 ms), with disjoint before/after sample ranges. A writer
dump is not called during those timed loops. The code-placement experiments
below reproduce and substantially reduce this regression without changing the
emitted simulation instructions.
Do not attribute the entire before/after difference to encoding or claim that
all controls are unchanged. The full matrix, ranges, commands, and binary
hashes are retained in `target/vcd-record-results/paired-256/`.

At 4,096 counters and 10,000 full cycles, five processes per engine give:

| Full-width case | Celox before (ms) | Complete records (ms) | Verilator (ms) |
| --- | ---: | ---: | ---: |
| idle | 22.072 | 21.636 | 113.053 |
| sparse | 29.795 | 28.660 | 113.871 |
| dense | 403.500 | 292.690 | 319.673 |

Dense elapsed time fell by 27.5%; new Celox took 8.4% less time than Verilator
in this run. Dense ranges were 378.755–428.553 ms before, 279.528–298.023 ms
after, and 301.363–334.930 ms for Verilator. Old and new Celox both emitted
2,826,428,895 timed bytes; Verilator emitted 2,837,328,892 bytes. The larger
fixture retains the sparse advantage. Results are in
`target/vcd-record-results/paired-4096/`. These results concern this two-state
counter fixture and `/dev/null`, not storage throughput or a general advantage
over Verilator on other designs.

The hardware-counter comparison runs 256 `full_width` dense counters for
1,000,000 cycles, rotating all three engines over five processes each.
`perf stat` counts user-space events for the whole process, including
construction; each event was counted for 100% of its enabled time. Medians:

| Engine | Instructions (billions) | Branches (billions) | Branch-miss rate |
| --- | ---: | ---: | ---: |
| Celox before | 70.391 | 13.218 | 0.061% |
| Complete records | 59.274 | 10.126 | 0.082% |
| Verilator | 30.073 | 2.949 | 0.176% |

Celox's instruction count fell by 15.8%, but remains 1.97x Verilator's in
this whole-process measurement. A 499 Hz profile of the new binary collected
1,154 samples with none lost: 67.3% self time was in `dump_with_activity`
and 5.7% in `memmove`. The earlier same-workload profile of the baseline
had 69.3% and 11.9% respectively. Profile shares are not absolute time
comparisons. Commands, environment, hashes, per-process counters, and the
profile are retained in `stat-manifest.json`, `stat-summary.json`, and
`profile-after.*` under `target/vcd-record-results/`.

Repeat the verified cached native comparison with:

```sh
python3 scripts/compare-vcd-verilator.py \
  --verilator target/verilator-v5.052/bin/verilator \
  --celox target/vcd-record-results/e2e-after \
  --baseline-celox target/vcd-record-results/e2e-before \
  --output target/vcd-record-results/paired-256 \
  --signals 256 --steps 100000 --repeats 5 \
  --patterns counter full_width --reuse-builds
```

For the larger run use `--signals 4096 --steps 10000 --patterns full_width
--modes vcd` and output directory `paired-4096`. Omit `--reuse-builds` for
fresh Verilator fixtures. `binaries.json` links the saved executables to the
runtime source snapshots; `output-equivalence.json` records the nine identical
before/after validation waveform bodies and equal timed byte counts.

### Investigation of the no-output regressions

The complete-record implementation is committed as `e45be5052`. Follow-up
experiments use the same saved baseline and optimized executables, CPU 0 on
the Ryzen 7 9800X3D, and `/dev/null`. Builds and other benchmarks were excluded
from timing runs. Code placement demonstrably affects both no-output controls;
the particular CPU frontend mechanism remains unmeasured.

First, an `mprotect` interposer captured the packed native JIT image before
execution. The old and new executables emit byte-identical images in each mode:

| Mode | Image bytes | SHA-256, identical before/after |
| --- | ---: | --- |
| off | 51,514 | `3400ee6c67fd55b389ceb4d3fb395811f870b7d80843b6103089b2a44d7a5174` |
| instrumented | 63,165 | `03f7393ad9710a01a7466b5cdb65ae9b91e724727746a9f79f51d5bb5ca4a045` |

A separate allocation interposer aligned the simulation state to either
64 bytes or 4 KiB. The instrumented regression remained at both alignments.
Whole-process instructions and branches were almost unchanged. Thus neither
additional emitted JIT instructions nor state alignment alone explains it.

Next, a diagnostic mapping interposer moved the entire instrumented image
within its executable allocation, retaining its bytes and relative entry
offsets. A five-process confirmation rotates all six version/placement
combinations, after one warm process each. At 256 full-width dense counters
and 5,000,000 cycles, medians are:

| JIT start offset from page boundary | Before (ms) | Complete records (ms) | Before cycles (billions) | Complete-record cycles (billions) |
| --- | ---: | ---: | ---: | ---: |
| 0 | 1,096.909 | 1,511.675 | 6.345 | 8.337 |
| 64 | 1,100.134 | 1,161.446 | 6.267 | 6.449 |
| 4,096 | 1,098.994 | 1,550.816 | 6.264 | 8.412 |

Elapsed times cover the simulation loop; hardware counters cover the whole
process. All six variants retire approximately 16.364 billion instructions
and 2.322 billion branches. Median branch misses range from 4.60 to 4.77 million;
the extra cycles at offset zero do not accompany extra branch misses. Moving
the image by 64 bytes reduces the elapsed-time regression from 37.8% to 5.6%.
Moving by 4 KiB restores it. A 64-byte move preserves every instruction's
position within a cache line, so individual loop alignment alone cannot
explain this result. Interaction with other code through frontend caches or
predictor indexing is a hypothesis consistent with these measurements.
All six placements produce the same 20-cycle waveform body as the previously
oracle-validated fixture. The initial three-process sweep also retains results
for offsets 0, 16, 32, 48, and 64; none were discarded from their summaries.

The standalone `collect` regression has a smaller reproducible example. Its
hot `TraceLayout::take` loop has the same 36-byte instruction sequence after
relocating branch destinations. In the baseline it starts at `0x1dec1`, offset
1 within a 64-byte line; after the writer change it starts at `0x1e021`, offset
33. In the latter position the flag comparison crosses the line boundary.
A diagnostic trampoline places this loop at either offset in a page 1 MiB
from the executable base. It retains all inner-loop instructions and adds
jumps only on entry/exit. At 16,384 signals, 64 bits, 1,000,000 same-value dumps,
one warm process and five rotated measured processes per variant give:

| Mode | Loop offset within line | Before (ms) | Complete records (ms) | Before cycles (billions) | Complete-record cycles (billions) |
| --- | ---: | ---: | ---: | ---: | ---: |
| collect | 1 | 347.377 | 339.390 | 1.764 | 1.739 |
| collect | 33 | 506.066 | 487.752 | 2.488 | 2.444 |
| dirty | 1 | 752.042 | 730.221 | 3.737 | 3.630 |
| dirty | 33 | 950.933 | 921.109 | 4.621 | 4.493 |

At matching placement the original before/after regression disappears. Moving
either version's loop by 32 bytes slows `collect` by 44–46% and `dirty` by
about 26%, with the same work and effectively unchanged instruction counts
within each binary/mode. Comparisons, changes, and bytes agree across variants.
These trampoline times include their added jumps and are not directly
comparable to the original benchmark's absolute times. An earlier trampoline
64 MiB away also showed placement sensitivity but had noisier samples and a
residual `collect` difference; those results remain in the raw records.

AMD's [Zen 5 optimization guide, sections 2.8.2–2.8.3 and 2.9.1](https://docs.amd.com/v/u/en-US/58455_1.00)
describes effects of loop/branch placement on prediction throughput and of
Op Cache misses on instruction supply. This supports investigating frontend
placement even when branch-miss counts remain low. The measurements above
establish placement sensitivity, not a specific Op Cache conflict or loss of
instruction fusion. Identifying that mechanism needs suitable frontend PMU
events beyond the generic counters used here. The observed offsets are
diagnostic interventions for these executables and this processor, rather
than a generally validated JIT padding policy.

Raw data and diagnostic sources are in the ignored directory
`target/vcd-followup-results/`: `jit-captures.json`, `align-state-summary.json`,
`shift-summary.json`, `shift-confirm-summary.json`, `loop-summary.json`, and
`loop-near-summary.json`. The confirmation manifests record every command;
summaries retain every measured sample, including outliers. `binaries.json`
pins the executables. The C interposers are `capture-jit.c`, `align-state.c`,
`shift-jit.c`, and `relocate-loop-near.c`; the latter two experiments are run
by `measure-shift-confirm.py` and `measure-loop-near.py` after compiling each
interposer with `gcc -O2 -Wall -Wextra -Werror -shared -fPIC`. Their fixed image
size and instruction offsets apply only to the saved fixtures. The benchmark
process receives `LD_PRELOAD` after `perf stat -- env`, so the diagnostic does
not patch the profiler itself. `shift-confirm-validation.json` records the
six identical validation waveforms.

### Next steps toward Verilator

The current 256-counter dense result needs a further 33.0% elapsed-time
reduction to match Verilator in that run. The 4,096-counter result already
has lower elapsed time, so both sizes remain necessary controls. The existing
activity tracking still provides the sparse advantage.

Grouping the new native profile's instruction addresses puts approximately
34% of whole-process cycle samples in 64-bit encoding, 27% in signal-loop
dispatch and scalar/integer comparisons, and 5% in suffix/output management.
Separate `memmove` samples account for 5.7%. These are sampling estimates,
including possible skid, rather than isolated costs or additive speedups.
The address ranges are recorded in `writer-regions.json`.

1. **Reduce 64-bit conversion instructions first.** The current SSE2 path
   broadcasts words, shifts/masks them, and packs bytes before testing bits.
   Verilator's [SSE2 conversion](https://github.com/verilator/verilator/blob/ea338be98e1e838d3518809ce8899f85a009963c/include/verilated_trace_imp.h#L474)
   uses byte unpacking and shuffles instead. Compare that approach with the
   current encoder while preserving leading-zero abbreviation, including
   short counters, full-width data, and unaligned output. Test an AVX2 path
   separately, selecting an entire encoding loop outside per-signal work.
   The compared Verilator fixture uses `-O3 -flto` without `-march=native` or
   `-mavx2`; its result does not require an AVX2 explanation. Even halving the
   sampled conversion cost would imply only about 17% overall improvement,
   so encoding alone is not an established solution to the remaining gap.

2. **Move repeated signal decisions into trace construction.** Signal metadata
   still has a 104-byte stride including names/scopes unused by normal dumps.
   A compact trace plan can separate header data, precompute same-type runs,
   and distinguish the initial snapshot from steady-state comparisons.
   Validate memory coverage before the loop, preserving the public slice
   bounds contract, and reduce repeated indexing checks. Verilator's
   [typed comparison primitives](https://github.com/verilator/verilator/blob/ea338be98e1e838d3518809ce8899f85a009963c/include/verilated_trace.h#L398)
   are called from generated fixed-offset code. Celox can first specialize
   its trace plan without runtime code rewriting, retaining registration
   order, aliases, external values, and the four-state fallback.

3. **Revisit the remaining output-buffer copy after those changes.** Complete
   records removed short-ID copy calls, while dump tails still pass through
   `BufWriter`. Eliminating all sampled `memmove` time would save only about
   5.7% in isolation. A single output buffer may also reduce surrounding
   bookkeeping, but must retain timestamp ordering, flush/Drop behavior,
   short-write handling, and I/O error propagation.

Use instruction counts together with elapsed-time distributions, preserving
`off`, `instrumented`, and `collect` controls. A source change can move unrelated
hot code even when the generated simulation image is identical. Benchmark
candidate loop layouts across circuit sizes and sparse/dense patterns before
adopting a padding policy. PGO can affect placement and inlining, but the
0.082% dense branch-miss rate gives little evidence for prioritizing branch
hints over removing work. The following stage implements and measures the
SSE2 conversion change in isolation; the trace-plan and buffer changes remain
subsequent candidates.

### Shared-byte SSE2 expansion

This stage uses the complete-record implementation (`e45be5052`, documented
in `32873bbae`) as the baseline. It duplicates the eight input bytes once and
uses two immediate shuffles per required 16-digit group. Explicit groups let
the compiler share the input expansion without a runtime shuffle selector.
Leading-zero abbreviation, output bounds and padding, suffix publication,
and the SSE2 requirement are retained. Signal selection, comparison, and
buffering are outside this change.

The first candidate replaced each 16-bit broadcast/pack with unpack/shuffle
operations but repeated the input expansion. It reduced writer instructions
by about 5%, while elapsed time was essentially unchanged (scan 490.325 to
484.478 ms, dirty 505.512 to 515.641 ms). The shared expansion below was selected
after a separate paired comparison. Both candidates and their raw samples
are preserved; the first candidate's executable and summaries use the `v1-`
prefix.

The final writer comparison uses 16,384 two-state 64-bit signals, 5,000 dense
dumps, CPU 0, `/dev/null`, one warm process and five alternating measured
processes per binary/mode. Comparisons, changes, and bytes are equal in each
pair: 81,920,000 comparisons and changes, and 5,689,308,893 timed bytes.
Hardware counters cover the whole process and run for 100% of enabled time.

| Mode | Before (ms) | Shared expansion (ms) | Before instructions (billions) | Shared-expansion instructions (billions) |
| --- | ---: | ---: | ---: | ---: |
| scan | 487.172 | 431.845 | 15.979 | 14.177 |
| dirty | 510.597 | 467.532 | 16.639 | 14.837 |

Elapsed time falls by 8.4–11.4%, with a 10.8–11.3% instruction reduction.

The negative controls are retained. Their dump counts are longer because
short idle/sparse samples are too small to assess reliably:

| Mode / case | Dumps | Before (ms) | Shared expansion (ms) |
| --- | ---: | ---: | ---: |
| dirty / idle | 5,000,000 | 183.925 | 233.011 |
| dirty / sparse_scattered | 100,000 | 101.930 | 90.917 |
| dirty / same_value | 100,000 | 89.742 | 76.900 |
| collect / same_value | 1,000,000 | 468.916 | 367.315 |

Same-value and `collect` improvements occur without executing the changed
encoder during timing. Conversely, idle regresses by 26.7% despite essentially
unchanged whole-process instructions (about 5.52 billion each). These are not
uniform encoder speedups. The generic-width/four-state matrix also retains
its results: 32-bit dense takes 232.573 versus 242.213 ms; the other measured
1/63/65/256-bit dense and 64-bit four-state cases range from a 4.8% decrease
to a 1.5% increase in median time. All pairs retain equal comparisons, changes,
and output bytes.

A 100,000,000-dump idle profile reproduces the regression (3.772 versus
4.545 seconds, no lost samples). Most additional cycle samples are in the
benchmark's activity-summary loop. Its 85-byte body is identical except
external branch displacements, but moves from `0x1df90` (offset 16 within a
64-byte line) to `0x1df80` (offset 0). The latter placement makes the memory
bounds comparison cross a line boundary. A diagnostic trampoline copies
this unchanged body to controlled offsets in a page 1 MiB from the executable
base; it adds entry/exit jumps and retains every inner-loop instruction.
At 5,000,000 idle dumps, one warm process and five rotated measured processes
per variant give:

| Mode | Loop offset within line | Before (ms) | Shared expansion (ms) |
| --- | ---: | ---: | ---: |
| collect | 0 | 141.161 | 136.827 |
| collect | 16 | 94.220 | 94.579 |
| dirty | 0 | 221.398 | 231.388 |
| dirty | 16 | 179.057 | 184.399 |

Matching placement removes the isolated collection regression. The complete
idle writer retains a 3.0–4.5% median difference under matching placement,
compared with 26.7% in the unmodified executables. This confirms a substantial
placement contribution, without identifying a specific frontend mechanism.
Trampoline timings include their added jumps; the production change contains
only the encoder improvement. The original idle regression remains a measured
limitation of these built executables.

The native benchmark is rebuilt and linked with the repository's bench/LTO
profile. The following comparison rotates the baseline, shared expansion, and
the same verified Verilator 5.052 executables over five measured processes
after warming each variant. CPU 0, `/dev/null`, initial-snapshot exclusions,
and the previous counter oracle apply. Dense medians:

| Counters / pattern / cycles | Before (ms) | Shared expansion (ms) | Verilator (ms) |
| --- | ---: | ---: | ---: |
| 256 / counter / 100,000 | 183.688 | 177.787 | 164.883 |
| 256 / full_width / 100,000 | 206.137 | 196.232 | 146.668 |
| 4,096 / full_width / 10,000 | 298.556 | 286.959 | 334.363 |

At 256 counters, dense elapsed time falls by 3.2% for short counters and 4.8%
for full-width values. The full-width ratio to Verilator narrows from 1.41x to
1.34x within this comparison. Its before range is 202.864–213.830 ms and after
range 193.003–199.543 ms. Native sparse medians are nearly equal (counter:
52.436 versus 52.529 ms; full-width: 51.486 versus 51.548 ms). Idle takes
39.416 versus 41.562 ms for counter and 40.741 versus 40.587 ms for full-width;
the before/after ranges overlap. These data do not establish a general idle
or sparse improvement.

The 4,096-counter matrix is much noisier: dense ranges are 287.173–321.475 ms
before and 265.330–604.975 ms after. Sparse ranges are 28.854–141.769 and
28.642–138.538 ms. Those samples remain in the reported medians; this matrix
alone does not establish a reliable larger-fixture speedup. The stable
256-counter result and retired-instruction reduction support this change.

The dense no-output controls also vary: counter `off` takes 14.931 versus
15.240 ms and `instrumented` takes 30.888 versus 29.981 ms; full-width takes
15.729 versus 15.021 ms and 30.961 versus 29.459 ms respectively. Captured JIT
images for both modes are byte-identical to the baseline, with the same hashes
reported in the preceding investigation. Do not subtract separate controls
to estimate isolated encoding time.

At 256 full-width dense counters and 1,000,000 cycles, five rotated whole-process
`perf stat` samples give:

| Engine | Instructions (billions) | Branches (billions) | Cycles (billions) |
| --- | ---: | ---: | ---: |
| Complete-record baseline | 59.274 | 10.126 | 11.850 |
| Shared expansion | 53.386 | 10.126 | 11.124 |
| Verilator | 30.073 | 2.949 | 7.444 |

Instructions fall by 9.9%; cycles fall by 6.1% in these medians. Branch counts
are essentially unchanged. A 499 Hz profile of the final native executable
has 1,105 samples with none lost: writer self time is 66.1%, with `memmove` at
5.8%. Grouping instruction addresses puts approximately 27% of whole-process
cycle samples in 64-bit encoding and 33% in signal-loop dispatch and
scalar/integer comparison. These are sampling estimates, including possible
skid, not isolated costs. The next target is the compact trace plan described
above: reduce per-signal decisions and repeated metadata/indexing work.

All three engines pass the waveform oracle; the nine old/new validation
waveform bodies are also byte-identical, and all timed byte counts match.
Validation includes the 11 runtime tests, 7 state-layout tests, 4 cross-backend
VCD integration tests, formatting, Clippy for all runtime targets with warnings
denied, and the aarch64 runtime check. The existing integer oracle covers every
significant bit length, zero, random values, and unaligned output; record tests
check that partial SIMD groups never publish their padding.

Raw samples, commands, source/binary hashes, JIT captures, and profiles are in
`target/vcd-shuffle-results/`. `compare-writer.py` accepts `dense`, `controls`,
and `fallback`; `run-comparisons.py` records the paired native matrix and
hardware-counter runs. `output-equivalence.json` records exact waveform and
byte-count agreement. `profile-idle.py`, `relocate-idle-loop.c`, and
`measure-idle-loop.py` retain the idle diagnosis; that shim is specific to the
saved executable offsets. The comparison can be repeated with:

```sh
python3 scripts/compare-vcd-verilator.py \
  --verilator target/verilator-v5.052/bin/verilator \
  --celox target/vcd-shuffle-results/e2e-after \
  --baseline-celox target/vcd-shuffle-results/e2e-before \
  --output target/vcd-shuffle-results/paired-256 \
  --signals 256 --steps 100000 --repeats 5 \
  --patterns counter full_width --reuse-builds
```

### Word-at-a-time activity collection

The baseline for this stage is `5dafc4835`, including shared-byte SSE2 expansion.
Instead of adjusting loop placement, the collector removes work from both
levels of the activity hierarchy. It first obtains bounded, disjoint flag and
summary slices, then tests eight bytes at once. Only nonempty summary words
lead to flag ranges; only nonempty flag words lead to group enumeration.

For a nonzero word, adding `0x7f` to each byte's low seven bits and ORing the
original word sets a high bit exactly where that byte is nonzero. There are no
carries between bytes. Clearing one low set bit per iteration visits those
positions in ascending address order. The word is loaded as little endian so
that order also holds on big-endian hosts. Short tails use byte loads. The
implementation uses safe slices, has no new CPU-feature requirement, and accepts
every nonzero flag value. It retains the notification ABI, raw-view fallback,
registration order, and independent consumption of clock notifications.

The selected implementation batches both levels with normal compiler inlining.
A summary-only candidate reduced idle instructions but left most sparse flag
scanning intact. An additional candidate prohibited inlining of `TraceLayout::take`;
that did not remove the standalone dense instruction increase and added idle
instructions, so it was not retained. Sources and executables for these trials
are preserved as `v1`, `v2` (selected), and `v3` under
`target/vcd-activity-results/`. No alignment directive or runtime code patch is
part of this change.

The original monolithic benchmark showed an additional 163.839 million
instructions in `scan / dense` after the collector change: about two per input
update, even though this mode never calls the collector. Disassembly shows
additional loads in the stimulus loop. Its original three-way idle comparison
is retained: complete records take 232.704 ms, shared SSE2 takes 279.659 ms,
and word collection takes 137.913 ms at 16,384 signals and 5,000,000 dumps.
Thus the large idle regression is removed even with the original harness.

The final writer comparison rebuilds all three library versions with the
isolated stimulus function. Its 278 instructions and 1,036-byte size agree in
all three executables after normalizing link-time addresses. Placement can still
vary. Across 130 correctness cases, the original stimulus, isolated stimulus,
and new collector produce identical waveform bodies and statistics. Absolute
timings and instruction counts from different harness revisions should not be
compared directly.

The following medians use CPU 0, `/dev/null`, 16,384 two-state 64-bit signals,
one warm process and five rotated measured processes per variant. Counters
cover the whole process, with no multiplexing. Builds and tests do not overlap
timing runs.

| Mode / case | Dumps | Complete records (ms) | Shared SSE2 baseline (ms) | Word collection (ms) |
| --- | ---: | ---: | ---: | ---: |
| dirty / idle | 5,000,000 | 209.895 | 190.694 | 126.135 |
| collect / idle | 5,000,000 | 99.363 | 95.290 | 29.498 |
| scan / dense | 5,000 | 534.144 | 484.899 | 483.634 |
| dirty / dense | 5,000 | 555.172 | 504.650 | 480.768 |

Idle collection instructions fall from 3.166 to 0.841 billion (73.4%); complete
idle writer instructions fall from 5.530 to 3.200 billion (42.1%). The complete
idle writer's before range is 188.992–234.011 ms and after range
118.923–137.412 ms. This improvement removes executed work rather than relying
only on favorable placement. Dense instruction counts are almost unchanged:
`scan` is 12.375 billion in both versions, and `dirty` is 13.107 versus 13.102
billion. The 4.7% dense median improvement is not a new encoding optimization;
its elapsed-time ranges overlap. The previous SSE2 instruction reduction is
retained under the same harness.

Sparse controls, comparing shared SSE2 with word collection:

| Mode / case | Dumps | Before (ms) | After (ms) | Before instructions (billions) | After instructions (billions) |
| --- | ---: | ---: | ---: | ---: | ---: |
| dirty / sparse_scattered | 100,000 | 109.105 | 68.232 | 2.160 | 1.453 |
| dirty / same_value | 100,000 | 105.027 | 63.975 | 2.021 | 1.314 |
| collect / same_value | 1,000,000 | 515.362 | 130.902 | 9.842 | 2.775 |

The size sweep retains an important limitation. At 256 signals, there is only
one summary byte, so a completely idle collector does not benefit from batching:
5,000,000 `collect / idle` dumps take 14.742 versus 19.983 ms, an increase of
about 1.05 ns per dump, with instructions rising from 0.366 to 0.521 billion.
Complete `dirty / idle` takes 109.310 versus 112.490 ms. At 4,096 signals,
complete idle takes 134.070 versus 119.515 ms; at 65,536 signals it takes
409.415 versus 140.617 ms. Empty collection at 65,536 signals falls from
11.572 to 1.747 billion instructions.

Dense timing is not uniformly better across sizes: 256 signals take 491.849
versus 508.344 ms, 4,096 take 460.039 versus 458.219 ms, and 65,536 take
459.933 versus 482.440 ms, each with 81,920,000 input updates. All three pairs'
elapsed-time ranges overlap, and their instruction counts differ by less than
0.1%. The generic-width/four-state dense controls range from a 1.5% decrease
to a 5.0% increase in median elapsed time, with overlapping ranges and nearly
unchanged instructions. Four-state `mask_only` improves from 230.820 to
190.531 ms. These controls remain in the raw results; this change is a sparse
collection improvement, not a universal dense timing improvement.

The native benchmark is rebuilt and linked with the bench/LTO profile. It
uses the unchanged native harness, 256 counters, 500,000 full cycles, and five
rotated measured processes after warming each engine. Verilator 5.052 uses
the same verified cached executables as the preceding comparison. VCD medians:

| Pattern / case | Shared SSE2 baseline (ms) | Word collection (ms) | Verilator (ms) |
| --- | ---: | ---: | ---: |
| counter / idle | 229.488 | 195.228 | 292.656 |
| counter / sparse | 296.781 | 263.442 | 303.707 |
| counter / dense | 987.490 | 992.998 | 792.315 |
| full_width / idle | 225.576 | 221.236 | 293.164 |
| full_width / sparse | 405.623 | 289.942 | 329.587 |
| full_width / dense | 1,078.541 | 1,060.526 | 781.193 |

Native dense is approximately preserved: counter changes by +0.6% and
full-width by -1.7%, with overlapping ranges. The full-width ratio to Verilator
is 1.38x versus 1.36x within this run; the dense gap remains. Idle and sparse
medians improve, but their ranges overlap too. In particular, full-width sparse
ranges are 269.614–433.173 versus 274.957–412.636 ms, so its large median
decrease should not be treated as a stable 29% native speedup.

Five rotated whole-process hardware-counter runs at 1,000,000 full-width cycles
provide separate evidence of the work reduction:

| Case | Before instructions (billions) | After instructions (billions) | Before cycles (billions) | After cycles (billions) |
| --- | ---: | ---: | ---: | ---: |
| idle | 10.266 | 9.830 | 3.114 | 2.841 |
| dense | 53.386 | 53.158 | 11.356 | 11.195 |

Native idle instructions fall by 4.2%; dense instructions fall by 0.4%.
The final dense profile attributes 66.5% of whole-process samples to writer
self time, 5.8% to `memmove`, and 1.8% to `TraceLayout::take`. This leaves
the per-signal comparison/dispatch and output paths as the main targets for
closing the dense Verilator gap.

The `off` and `instrumented` controls are retained in the native results.
Captured JIT images are byte-identical to the preceding stage in both modes,
including the recorded hashes. No-output timings still vary (full-width dense
`instrumented`: 148.837 versus 120.723 ms); they do not measure the new collector
and must not be subtracted to estimate its cost.

All three engines pass the waveform oracle at 256 and 4,096 counters. The nine
old/new validation waveform bodies and all native timed byte counts agree.
The larger native fixture is checked for correctness only in this stage;
there is no new 4,096-counter native timing claim. Validation includes
11 runtime tests, 9 state-layout tests, 4 cross-backend VCD integration tests,
formatting, Clippy for all runtime/state-layout targets, and the aarch64 runtime
check. The new tests cover zero-sized layouts, summary/word boundaries, partial
tails, all byte values, unaligned metadata, stale result vectors, and untouched
memory outside the consumed activity.

Artifacts are in `target/vcd-activity-results/`. The selected library sources,
original-harness binaries, native binaries, JIT captures, and profiles remain
at that level. `isolated-writer/` contains the final writer comparison, all three
rebuilt executables, source hashes, instruction-sequence comparison, and 130
stimulus-equivalence checks. Its `compare-writer.py` accepts `regression`,
`controls`, `scale`, and `fallback`. Reproduce the native comparison with:

```sh
python3 scripts/compare-vcd-verilator.py \
  --verilator target/verilator-v5.052/bin/verilator \
  --celox target/vcd-activity-results/e2e-after \
  --baseline-celox target/vcd-activity-results/e2e-before \
  --output target/vcd-activity-results/paired-256 \
  --signals 256 --steps 500000 --repeats 5 \
  --patterns counter full_width --reuse-builds
```

### Typed trace plans

This stage starts from `15f179813046fbaf2ca974686232cd16b0fb3d7c`.
The preceding 256-counter full-width dense comparison was 1.36 times Verilator's
elapsed time. Its whole-process profile used 53.158 billion instructions,
versus Verilator's 30.073 billion, with only 0.088% of Celox's branches missed.
Correctly predicted branches still execute their surrounding loads, indexing,
and bookkeeping. The generated Verilator trace calls fixed-offset, typed
comparison primitives; the old Celox writer revisited signal kind, initialization,
previous-plane offsets, and slice bounds on every comparison. This stage targets
that repeated work without changing the write-notification ABI or generated
simulation code.

Registration now builds separate bit, 64-bit, and generic record arrays, plus
a registration-order map and consecutive runs. On this x86-64 build, a fixed
record's hot stride is 40 bytes instead of 104, and includes its previous integer.
That is a hot-record measurement: header strings, the registration map, and run
storage still occupy separate allocations. Generic records retain packed
previous value/mask planes. Initial values have a separate loop specialization.
Each dump checks memory coverage before fixed-width reads; sparse callers may
still supply a slice covering only their selected signals. The plan retains
scope/ID assignments, registration order, aliases, and external-value behavior.

Two details matter for the generated loop. Comparison statistics accumulate
once per completed run; a returned I/O error counts the visited prefix, including
unchanged signals. Value encoding and suffix publication stay in the same
inlined loop, allowing SIMD constants to remain in registers across records.
A plain inline hint did not accomplish this in the first experiment. An empty
selection returns after timestamp handling, avoiding additional idle traversal.

The first run-based candidate regressed on alternating types: a 16,384-signal
`1,64` dense scan took 714 versus 494 ms, and a full idle scan took 1,244 versus
540 ms. Each one-record run paid for a call and a variable-length inner loop.
Inlining only the run dispatcher subsequently outlined the fixed-width helper
and retained much of that cost. The next candidate also inlines the
fixed-width loop; when average run length is below two, it walks registration
entries with a known one-record length. This candidate still coalesces sparse
selections and uses long runs where available.

Alternating-width controls use the same fixture source and compiler options
for both writers, with width read per descriptor by the isolated stimulus
function. The following are five rotated process medians on CPU 0, `/dev/null`,
16,384 signals, 5,000 dense dumps or 20,000 full-scan idle dumps. Instruction
counts include process construction; timings exclude initialization and include
the final flush. These standalone mixed-width timings are separate from the
Cargo writer benchmark and linked native comparison.

| Width pattern | Mode/case | Previous ms | Plan ms | Previous instructions B | Plan instructions B |
| --- | --- | ---: | ---: | ---: | ---: |
| 1,64 | scan/dense | 506.558 | 448.520 | 12.177 | 10.294 |
| 1,64 | dirty/dense | 564.828 | 463.015 | 12.919 | 11.036 |
| 1,64 | scan/idle | 729.311 | 283.850 | 13.370 | 7.146 |
| 1,9,64,65 | scan/dense | 1,281.307 | 1,238.415 | 22.117 | 21.053 |
| 1,9,64,65 | dirty/dense | 1,100.067 | 1,062.876 | 22.902 | 21.839 |
| 1,9,64,65 | scan/idle | 1,113.012 | 904.861 | 26.397 | 20.010 |

Host timing variation remains substantial. The repeated instruction reductions
are stronger evidence of removing work than any single elapsed-time ratio.
Raw samples, rejected candidates, sources, and executables are retained under
`target/vcd-plan-results/`; `v7-mixed-summary.json` records these mixed controls.
The normal benchmark's stimulus remains exactly 278 instructions and 1,036 bytes
in both builds after relocation normalization, so changing the trace plan did
not change its per-signal input update loop.

The first linked native comparison reached 1.009 times Verilator on full-width
dense output (805.949 versus 798.525 ms; the previous writer took 1,124.583 ms).
However, full-width idle rose from 205.309 to 217.424 ms and sparse from 260.209
to 305.969 ms. Whole-process idle instructions increased from 9.830 to 10.550
billion. This was not solely host timing noise and required a sparse-path fix.

The actual clock home is byte 2,080. Its 64-byte group contains `clk`, `rst`,
`en0` through `en29`, and `q252` through `q255`: 36 trace entries. Registration
sorts names lexically, so `en10` is followed by `en100`, and most of the selected
bit indices have gaps. Those 36 entries formed 25 runs. A uniform bit fixture
did not reproduce the extra work because its selected indices were consecutive.
The final sparse path visits each selected entry once with a known one-record
length, removing both run reconstruction and dynamic loop setup at these gaps.

The diagnostic exports the native fixture's real descriptors, initial memory,
counter increments, and both half-cycle activity lists. A separate writer
executable replays the same clock and counter updates with those activity lists;
it excludes scheduling and activity collection. Five rotated process medians
at one million full cycles give:

| Native-layout replay | Previous ms | Coalescing candidate ms | Direct sparse ms | Previous instructions B | Direct sparse instructions B |
| --- | ---: | ---: | ---: | ---: | ---: |
| idle | 218.716 | 263.995 | 174.662 | 5.541 | 4.178 |
| sparse | 335.887 | 386.632 | 296.366 | 7.451 | 5.953 |
| dense | 2,856.165 | 1,579.256 | 1,608.278 | 48.792 | 32.342 |

The intermediate candidate and final sparse-path experiment are retained
separately under `target/vcd-plan-results/` and
`target/vcd-plan-sparse-results/`. The latter includes `capture-fixture.rs`,
the three `native-*.json` fixtures, `replay.rs`, compiler commands, immutable
executables, and `replay-summary.json`. All replay variants use the same input
update function and fixture. Their comparison, change, and value-byte counts
agree; replay elapsed times are not end-to-end simulation timings.

The final library was rebuilt and linked into the native benchmark with the
normal release/LTO settings. On the Ryzen 7 9800X3D/WSL host, the final paired
comparison uses 256 counters, 500,000 full cycles, CPU 0, `/dev/null`, a 64 MiB
stack, one warm process, and five rotated measured processes per engine/mode.
Construction, reset, and the initial snapshot are excluded; the final flush is
included. Both Verilator fixtures reuse binaries verified against the preceding
manifest. The previous Celox executable is the unchanged `15f179813` baseline.

| Pattern | VCD case | Previous ms | Final ms | Verilator ms | Final / Verilator |
| --- | --- | ---: | ---: | ---: | ---: |
| counter | idle | 214.647 | 199.438 | 298.732 | 0.668 |
| counter | sparse | 281.895 | 251.754 | 318.834 | 0.790 |
| counter | dense | 933.726 | 712.029 | 817.287 | 0.871 |
| full_width | idle | 196.687 | 181.480 | 290.215 | 0.625 |
| full_width | sparse | 262.491 | 227.740 | 308.605 | 0.738 |
| full_width | dense | 1,055.644 | 788.720 | 815.000 | 0.968 |

The full-width dense ratio is 1.295 before and 0.968 after in this comparison,
a 25.3% reduction in Celox elapsed time. The previous 1.36 ratio came from an
earlier run and should not be combined with these timings. Final dense samples
range from 765.277 to 801.541 ms; Verilator ranges from 764.966 to 866.668 ms.
These measurements support approximate parity on this fixture, not a stable
3.2% lead across workloads. The two Celox versions emit exactly 8,842,888,899
timed bytes in this case; Verilator emits 8,827,888,896. Counter-pattern output
still benefits from leading-zero abbreviation and is a different volume control.

Whole-process hardware counters at one million full-width cycles include
construction, so their cycle ratios are not steady-state timing ratios. Five
rotated repetitions of the old writer, intermediate candidate, final writer,
and Verilator give these instruction medians:

| Case | Previous B | Coalescing candidate B | Final B | Verilator B |
| --- | ---: | ---: | ---: | ---: |
| idle | 9.830 | 10.550 | 8.270 | 8.800 |
| sparse | 11.785 | 12.530 | 10.103 | 8.966 |
| dense | 53.158 | 36.933 | 35.535 | 30.073 |

The final reductions from the previous writer are 15.9%, 14.3%, and 33.2%.
The repeated instruction counts also verify that the sparse fix removes work
from the real simulator. Timing variation remains visible in unchanged-code
controls: full-width idle `off`, for example, moves from 63.195 to 67.221 ms.
`off` and `instrumented` JIT captures remain byte-identical to the baseline;
these control timings are not subtracted to estimate writer cost.

The final writer-only checks cover 256 through 65,536 signals, widths 1/32/63/65/256,
64-bit four-state values, mask-only changes, no changes, same-value stores, and
sparse updates. All dense configurations reduce instructions relative to the
previous writer; `collect` instruction counts change by less than 1%.
Empty-selection instruction counts are within 0.2% of the previous writer in
the 256/4,096/16,384/65,536 signal controls. Raw elapsed samples are retained,
including controls whose medians increase despite similar instruction counts.

Validation passes 15 runtime tests, 9 state-layout tests, 4 cross-backend VCD
integration tests, formatting, Clippy on all runtime/state-layout targets, and
the aarch64 runtime check. All 130 writer waveform/statistics comparisons pass.
The native-layout replay matches both linked Celox executables byte-for-byte
after the header, including comparison/change/value-byte counts, in all three
cases. The three engines pass the waveform oracle at 256 and 4,096 counters;
the larger native fixture is a correctness check only in this stage. Native
old/new timed byte counts also agree in every measured case.

Final artifacts and all commands are in `target/vcd-plan-sparse-results/`.
`candidate-manifest.json`, `checks.json`, and `final-runs.json` record sources,
executables, validation, and measurement commands. Reproduce the native matrix:

```sh
python3 scripts/compare-vcd-verilator.py \
  --verilator target/verilator-v5.052/bin/verilator \
  --celox target/vcd-plan-sparse-results/e2e-after \
  --baseline-celox target/vcd-plan-sparse-results/e2e-before \
  --output target/vcd-plan-sparse-results/paired-256 \
  --signals 256 --steps 500000 --repeats 5 \
  --patterns counter full_width --reuse-builds
```

## Correctness

`cargo test --locked -p celox-runtime -p celox-state-layout` checks byte encoding
against an independent BigUint oracle, padding, mask-only transitions,
registration order, aliases, activity consumption, and flush errors. It also
checks integer formatting at every significant bit length, unaligned input and
output, exact end-of-buffer loads, output capacity growth/reuse, and aliases of
64-bit values with repeated unchanged samples. Suffix tests cover ID carries,
the inline/long boundary, adjacent records of different significant lengths,
and bounds on fixed-store padding. Scalar-only and mixed-width traces cross
small block sizes while retaining registration order and scope/ID mappings.
The output tests cover
output-block boundaries, records larger than a block, short writes,
interrupted writes, partial I/O failures, and the final tail flushed by Drop.
Trace-plan tests cover mixed typed/generic runs, external values, repeated
timestamps, short selected-memory slices, rejected out-of-bounds ranges,
initial-snapshot retries, and comparison counts at an I/O-error boundary.
`cargo test --locked -p celox --test vcd` compares parsed incremental output with
full scans of the same committed state across native, Cranelift, interpreter,
Wasm, and tiered backends, with optimization and four-state mode on and off. It
also exercises multiple ticks per dump, repeated timestamps, dynamic array
writes, retained raw views, and clock-trigger consumption.
`PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -p
'test_compare_vcd_verilator.py'` checks that the external comparison rejects
incorrect values, sample times, timescales, and signal sets while accepting
equivalent VCD padding and empty timestamps. It also checks that replacing a
cached executable preserves the original baseline when input/output paths alias.
