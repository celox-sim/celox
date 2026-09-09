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

The writer keeps previous value and mask planes as packed bytes. Padding above a
signal's declared width is ignored. Four-state and external signals compare
both planes; two-state memory signals compare only values because their previous
mask stays zero for the writer's lifetime. Two-state memory signals of widths
1 and 64 are classified when registering signals and use fixed-size integer
comparisons and copies, including unaligned memory and previous-value offsets.
The one-bit path emits its ASCII digit directly. On x86-64 with SSE2 enabled,
the 64-bit path expands 16 bits at a time directly into spare output capacity;
other targets retain the scalar lookup-table encoder. It publishes only the
significant digits, preserving leading-zero abbreviation and the single digit
for zero. Other widths and four-state/external values use the generic byte
encoder; unknown bits retain VCD's `x`/`z` distinction. A changed vector
emits its complete VCD value, even if
only one bit changed. Values, IDs, and newlines are appended directly to a
reusable output block. Blocks at least as large as the `BufWriter` capacity
(256 KiB) bypass its internal copy; a dump's final partial block is handed to
`BufWriter` before returning. Headers, timestamps, and value records therefore
retain their order, including when a single record exceeds the block size.
The block reserves capacity for 256 KiB plus the largest possible record at the
first dump, adding roughly 256 KiB compared with a single-record buffer.
Header construction stays in a separate cold function. The pending change
count belongs to the output block, so unchanged-signal comparisons need not
carry a separate local accumulator through the loop.

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
CSV reports wall time, comparisons, changed signals, and actual emitted bytes;
`scan` and `dirty` must have identical change counts and output byte counts.

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

## Correctness

`cargo test --locked -p celox-runtime -p celox-state-layout` checks byte encoding
against an independent BigUint oracle, padding, mask-only transitions,
registration order, aliases, activity consumption, and flush errors. It also
checks integer formatting at every significant bit length, unaligned input and
output, exact end-of-buffer loads, output capacity growth/reuse, and aliases of
64-bit values with repeated unchanged samples. The output tests cover
output-block boundaries, records larger than a block, short writes,
interrupted writes, partial I/O failures, and the final tail flushed by Drop.
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
