# Benchmarks

Celox tracks compile time and simulation throughput against its own backends and
Verilator. The dashboard is useful for trends; it is not a universal prediction
for every RTL design.

## Dashboard

<ClientOnly><BenchmarkDashboard /></ClientOnly>

The complete benchmark matrix and raw history are available on the
[external dashboard](https://celox-sim.github.io/celox/dev/bench/).

## Workloads

The main view prioritizes complete hardware workloads: Heliodor and representative
SHA-256, FFT and LZ77 circuits, plus rica's five self-checking RISC-V kernels.
The **rica** tab compares Celox tiered and Veryl simulator tiered by total process
time; its daily workflow runs at 04:47 UTC. Details and tarunes qualification
are tracked in the [circuit collection](./collection.md), including pinned sources,
acceptance checks and the planned SystemVerilog frontend comparisons.

[API, counter and standard-library measurements](./diagnostics.md) remain available
as diagnostics. Compilation and execution are reported separately; a faster
compile does not imply faster generated code.

The [Taxi / cocotb comparison](./taxi.md) runs daily at 03:17 UTC and publishes
Celox/Verilator results for five fixed configurations (472 tests per sample).
Its tab separates test-body, whole-process and HDL compilation time.

The [RTLMeter comparison](./rtlmeter.md) measures cold source-to-finish latency for
XuanTie-E902/E906 and HummingbirdV2-E203 hello programs in separate 2-state and 4-state charts, with
Verilator and the stable xezim release pinned by Nix. Unsupported modes and
failed qualifications remain visible in its status tables.

## Reading results

The regular Benchmark workflow runs daily at 01:47 UTC (10:47 JST) and can also
be dispatched manually. It runs Rust, Verilator, and TypeScript sequentially
in one job, so backend comparisons within that run share a VM and CPU. Each
published result records the CPU model that produced it, and every chart keeps
one series per backend and CPU model: the legend names the model, the line style
separates models of the same backend, and results published before the model was
captured appear as `CPU not recorded`. The `bench-host` artifact retains the
full runner identity. Separate workflow runs can receive different CPUs; use
history to spot trends rather than to establish small changes between commits.
The [Heliodor suite](./heliodor.md#expanded-linux-suite)
groups backends on one host where runtimes allow it; ARM four-hart comparisons
use two pairs, and eight-hart runs use separate jobs. Only results within the same
group share a CPU.

Benchmark measurements run on the daily schedule or by manual dispatch. PRs
exercise the benchmark tooling when it changes. Each measurement workflow keeps
its running sample and at most one pending run, replacing an older pending run
with the latest request. The regular benchmark, Heliodor and Taxi use independent
queues; their publishers retry against the latest `gh-pages` history so concurrent
publication preserves every history. Only `master` publishes history.

- Compare the same workload, backend, revision, and host environment.
- Treat small changes on shared CI runners as noise until repeated.
- Use long-running execution measurements for throughput conclusions.
- Include simulator construction when evaluating developer iteration time.
- Validate any optimization choice on the design it will actually run.

Heliodor uses an additional fixed-input acceptance workload. Its methodology is
described in [Heliodor Linux Benchmark](./heliodor.md). The dashboard compares
the native backend with synchronous Veryl-CC, and compares Celox and Veryl-CC
tiered execution by total time through completion, including compilation. Standalone Cranelift boot
results are excluded because their much longer runtime makes this chart
ineffective for that comparison.
Heliodor charts are separated by CPU architecture because results from different
runner types are not directly comparable, and every series within a section is
split by CPU model the same way. Every chart uses a zero baseline.

## Run locally

```bash
# CodSpeed benchmarks, as CI runs them. The rtlmeter benchmark runs the
# cases laid out in CELOX_RTLMETER_LAYOUT (PyYAML; --optional adds
# BlackParrot) and none without it.
python3 scripts/run-rtlmeter-bench.py prepare work/rtlmeter
python3 scripts/run-rtlmeter-bench.py codspeed-layout work/rtlmeter work/rtlmeter-layout
export CELOX_RTLMETER_LAYOUT=$PWD/work/rtlmeter-layout
cargo install cargo-codspeed --locked --version 5.0.1
cargo codspeed build --locked -p celox --features cranelift --bench compilation --bench scaling --bench rtlmeter
cargo codspeed run -p celox --bench compilation --bench scaling --bench rtlmeter

# Rust benchmarks
cargo bench -p celox

# TypeScript and N-API benchmarks
pnpm bench

# Verilator comparison (requires Verilator and a C++ toolchain)
bash scripts/run-verilator-bench.sh

# VCD comparison (also requires Python 3; validates matching waveforms)
python3 scripts/compare-vcd-verilator.py
```

The [VCD benchmark methodology and results](../internals/vcd-performance.md#verilator-comparison)
cover idle, sparse, and dense recording, with tracing disabled and enabled.

The CodSpeed workflow runs on CodSpeed's ARM64 Macro Runner
(`codspeed-macro-arm64-graviton-ubuntu-22-04`) with the walltime instrument:
on every push to `master`, daily at 02:17 UTC (11:17 JST), on manual dispatch,
and on pull requests labelled `bench` (`bench-blackparrot` also measures
BlackParrot). Other pull requests and merge groups do not run it, because one
run takes one to two hours of the runner. Results are compared with earlier
measurements using the repository's CodSpeed regression thresholds. On
`master` and `develop`, execution failures or a failed performance analysis
open or update the issue `CodSpeed is failing on <branch>`; the next successful
execution and analysis close it. Missing or incomplete analysis is reported as
a failure, rather than treated as healthy. The issue includes the run link and,
for regressions, the performance comparison and benchmark details. The local
command only checks that the benchmark suite runs.

| Benchmark | What it measures | Regression it guards |
|---|---|---|
| `compile/*`, `scaling/nested_write/*`, `compile/sorter_tree/*` (`compilation.rs`) | Build time of Veryl designs | Compile-time slowdowns and super-linear passes |
| `scaling/clock_alias/{compile,sim}/n{16,1024}` (`scaling.rs`) | N flops, each clocked by its own `assign l1clk = clk` while the testbench senses `clk` at negedge (VeeR-EH1 has 1,451 such copies) | Per-event scheduler cost that grows with the number of clock domains |
| `scaling/gated_clock/{compile,sim}/n{16,1024}` | N flops behind clock gates (`en_ff` captured at negedge, `gclk = clk & en_ff`) | Cost per gated or derived clock |
| `fallback/<construct>/sim` vs `fallback/baseline/sim` | 64 combinational lanes plus ONE comb `$display`, `assert final`, DPI-C call in `always @*`, `string` array or `final` block | One construct moving the whole design to a slower path: no settle skipping or partitioned settle, one compiled run per slot, the reference walk. Each should stay close to the baseline |
| `rtlmeter/<case>/compile` (`rtlmeter.rs`) | `celox run` analysis and code generation of VeeR-EL2, VeeR-EH1 and XuanTie-E906 CoreMark (BlackParrot 1x1 with `bench-blackparrot` or nightly) | Compile-time regressions on real designs |
| `rtlmeter/<case>/cycle` | Execution time of a fixed `+max_cycles` window divided by its cycles (native, O2, two-state, one thread; includes the testbench start-up such as program loading) | Per-cycle regressions on real designs, including a construct that slows the whole design |

Simulation benchmarks (`sim`) report the time of 1,000 clock cycles. The
RTLMeter cases come from `benchmarks/rtlmeter/codspeed_manifest.json` (pinned
RTLMeter revision, windows and rounds); every run must reach its window
without a testbench failure, or the benchmark fails.
A run whose resident memory passes `CELOX_RTLMETER_MEMORY_LIMIT_MIB` (20 GiB
in CI; VeeR-EH1 peaks at about 7 GB) or whose time passes
`CELOX_RTLMETER_TIMEOUT_SECS` is killed and fails its benchmark, and each run
prints its start and peak memory. The workflow runs without CodSpeed's
walltime profiler: it sampled every benchmarked process, the celox commands
included, with 8 KiB stack copies for tens of minutes, and the runner (32 GB,
16 Cortex-A72 cores) lost its connection. The build and the run print memory
and disk use every minute (`scripts/with-resource-log.sh`).

Local measurements are most useful for comparing two revisions on the same
machine. CI history is better for long-term trends than for small one-off deltas.
