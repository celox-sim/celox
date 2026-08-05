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
| Heliodor Linux | Whole-design generated-code throughput on a large external design |

Compilation and execution are reported separately. A faster compile does not
imply faster generated code, and a microbenchmark result does not establish
whole-design performance.

## Reading results

- Compare the same workload, backend, revision, and host environment.
- Treat small changes on shared CI runners as noise until repeated.
- Use long-running execution measurements for throughput conclusions.
- Include simulator construction when evaluating developer iteration time.
- Validate any optimization choice on the design it will actually run.

Heliodor uses an additional fixed-input acceptance workload. Its methodology is
described in [Heliodor Linux Benchmark](./heliodor.md).

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
```

The CodSpeed workflow runs on pull requests, merge queues, and `master`. Pull
requests are compared with the `master` baseline using deterministic CPU
simulation, while the local command only checks that the benchmark suite runs.

Local measurements are most useful for comparing two revisions on the same
machine. CI history is better for long-term trends than for small one-off deltas.
