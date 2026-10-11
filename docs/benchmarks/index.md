# Benchmarks

Celox tracks compile time and simulation throughput against its own backends and
Verilator. The dashboard is useful for trends; it is not a universal prediction
for every RTL design.

## Dashboard

<ClientOnly><BenchmarkDashboard /></ClientOnly>

The complete benchmark matrix and raw history are available on the
[external dashboard](https://celox-sim.github.io/celox/dev/bench/).

## Workload groups

| Group | What it exercises |
|---|---|
| Compile time (CodSpeed) | End-to-end frontend, optimization, layout, and native/Cranelift code generation |
| Counter | Sequential state updates and clock-event overhead |
| Standard library | A mix of combinational, sequential, and structured datapaths |
| TypeScript testbench | N-API calls, typed signal access, and scheduler overhead |
| Verilator comparison | Equivalent generated simulators for a reference baseline |
| Heliodor Linux | Whole-design compilation, execution including the host testbench, and tiered startup/total time |

Compilation and execution are reported separately. A faster compile does not
imply faster generated code, and a microbenchmark result does not establish
whole-design performance.

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
with the latest request. The regular benchmark and Heliodor use independent
queues; their publishers retry against the latest `gh-pages` history so concurrent
publication preserves both sets of results. Only `master` publishes history.

- Compare the same workload, backend, revision, and host environment.
- Treat small changes on shared CI runners as noise until repeated.
- Use long-running execution measurements for throughput conclusions.
- Include simulator construction when evaluating developer iteration time.
- Validate any optimization choice on the design it will actually run.

Heliodor uses an additional fixed-input acceptance workload. Its methodology is
described in [Heliodor Linux Benchmark](./heliodor.md). The dashboard compares
the native backend with synchronous Veryl-CC, and shows startup and end-to-end
time separately for Celox and Veryl-CC tiered execution. Standalone Cranelift boot
results are excluded because their much longer runtime makes this chart
ineffective for that comparison.
Heliodor charts are separated by CPU architecture because results from different
runner types are not directly comparable, and every series within a section is
split by CPU model the same way. Every chart uses a zero baseline.

## Run locally

```bash
# Compile-time benchmarks with CodSpeed
cargo install cargo-codspeed --locked --version 5.0.1
cargo codspeed build --locked -p celox --bench compilation
cargo codspeed run -p celox

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

The CodSpeed workflow runs daily on the default branch at 02:17 UTC (11:17 JST)
and supports manual dispatch on any branch. Pull requests, merge groups, and
pushes do not run it. Deterministic CPU simulation results are uploaded to
CodSpeed for comparison with earlier measurements using the repository's CodSpeed
regression thresholds. On `master` and `develop`, execution failures or a failed
performance analysis open or update the issue `CodSpeed is failing on <branch>`;
the next successful execution and analysis close it. Missing or incomplete
analysis is reported as a failure, rather than treated as healthy. The issue
includes the run link and, for regressions, the performance comparison and
benchmark details. The local command only checks that the benchmark suite runs.

Local measurements are most useful for comparing two revisions on the same
machine. CI history is better for long-term trends than for small one-off deltas.
