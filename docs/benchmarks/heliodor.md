# Heliodor Linux Benchmark

Heliodor is Celox's large external Veryl workload. It boots a pinned Linux image
and compares Celox's native and tiered JIT backends with synchronous and tiered
Veryl-CC using the same design revision and input workload on each architecture.
Celox tiered starts on the interpreter while native code is generated in the
background. Veryl-CC tiered starts on Cranelift and switches to C code as its
background compilation completes, matching `veryl test --backend cc`'s default
`aot_c_async=true` setting. The synchronous Veryl-CC runner explicitly sets
`aot_c_async=false` and waits for C compilation before simulation. Standalone
Cranelift Linux boot measurements are not collected or published because their
runtime is outside the useful scale of this comparison.

## What the benchmark answers

The benchmark separates three questions:

1. How long do the synchronous backends take to compile the design?
2. How long does the complete workload take to execute, including the testbench?
3. How long does tiered execution take from startup through Linux completion
   while compilation overlaps simulation?

Execution time includes the testbench for both simulators.
Tiered results have separate charts for startup until simulation begins, execution
with background compilation, and total time through Linux completion. The tiered
execution interval includes time on the initial backend and must not be read as
compiled-code throughput. Veryl-CC may finish on Cranelift before C compilation
completes; these runs remain valid tiered measurements. Startup and total time
begin before design analysis; source-file loading and building the benchmark
executables with Cargo are excluded.
The TSV retains `compile_elapsed_ns` for tiered startup, not the total background
compiler time. Each synchronous and tiered Veryl-CC run uses its own empty AOT-C
cache. Historical synchronous Veryl-CC measurements keep their original series.
A partial boot, projected completion time, or compile-only result is not a
successful execution result.

## Valid result

A run is accepted only when it:

- uses the pinned Heliodor and workload revisions;
- reaches the configured Linux completion marker;
- records compilation and execution separately;
- compares runners built from the intended Celox and Veryl revisions;
- preserves the logs needed to diagnose a timeout or semantic mismatch;
- proves that Celox tiered promoted and executed at least one generated-code
  evaluation before Linux completed;
- confirms that Veryl-CC tiered enabled asynchronous C compilation and executed
  at least one compiled or fallback dispatch.

This fixed completion marker prevents faster failures or incomplete boots from
being reported as performance improvements.

## Run locally

```bash
bash scripts/run-heliodor-bench.sh run
```

To compare both tiered backends:

```bash
HELIODOR_RUNNERS="celox-tiered veryl-cc-tiered" bash scripts/run-heliodor-bench.sh run
```

The fixed CI `gate` runs `veryl-cc-sync`, `celox`, `celox-tiered`, and
`veryl-cc-tiered` on x86-64. The nightly AArch64 job measures the same four
backends. Publishing requires both tiered results for each architecture.

The first run needs network access to obtain the pinned Heliodor checkout. The
script prints the selected revisions, build configuration, completion status,
and timings. Use the same machine and configuration for before/after comparisons.

Published results appear in the **Heliodor Linux** section of the
[benchmark dashboard](./index.md).

## Expanded Linux suite

Nightly and non-profiling manual runs also measure the following workloads on
x86-64 (`ubuntu-24.04`) and AArch64 (`ubuntu-24.04-arm`), using all four backends:

| Guest Linux kernel | Hart counts |
| --- | --- |
| 5.15 | 1, 2, 4, 8 |
| 6.6 | 1, 2, 4 |
| 7.1 | 1 |
| 7.1 with vector enabled | 1 |

These 9 workloads use Heliodor revision
`6285682fa0a514077da9d17fee385c7841160025`. Kernel versions refer to the
simulated guest, not the benchmark host OS. Backends run in separate jobs (72 jobs
for the complete suite). These jobs use separate hosted machines; their CPU and
memory details are retained in each artifact for interpreting comparisons.
Each runner has a one-hour timeout for 1/2 harts,
three hours for 4 harts, and four hours for 8 harts;
timeouts and incomplete runs fail the job and are not published as timings.
The nightly publisher requires the complete suite on both architectures.
The suite applies testbench adjustment `8hart-100m-v1`: the 8-hart Veryl test
gets the same 100-million-cycle budget as its upstream Verilator wrapper,
replacing the stale 30-million-cycle limit. The shutdown assertion is unchanged.
`HELIODOR_SUITE=1` applies this adjustment only to the pinned suite revision;
the selected adjustment is recorded in the job log.

Linux 7.1 SMP (2/4 harts) is excluded pending an upstream RTL fix. Both
configurations stop retiring instructions on one hart; the two-hart case also
reproduces on Verilator, with a stalled data-cache read and another hart waiting
for a lock. These failures are not counted as successful benchmark results.

The separate HEAD compatibility job skips only upstream revision
`94e9c5821c24a8941c3ddc3b76daddc7124a855a`: its testbench lacks the
`initial_assign` annotations for ROM/DRAM preloads added by `6285682`.
CI records this as a known-source exclusion, not a successful simulation.
Every other upstream HEAD remains eligible for the compatibility test.

The dashboard labels each kernel and hart count separately. Expanded results
use separate history from the older fixed gate because the design revision is
different. Each chart retains the same compilation, execution, and tiered timing
definitions described above. These large jobs do not run on pull requests.

For example, to run the Linux 6.6 four-hart workload locally:

```bash
HELIODOR_REF=6285682fa0a514077da9d17fee385c7841160025 \
HELIODOR_TESTS=test_soc_66_smp_linux_boot_4hart \
HELIODOR_RUNNERS="veryl-cc-sync celox celox-tiered veryl-cc-tiered" \
HELIODOR_CELOX_CARGO_PROFILE=release HELIODOR_TIMEOUT_SEC=10800 \
bash scripts/run-heliodor-bench.sh run
```

For a focused manual rerun, set `suite_test`, `suite_runner`, and/or `suite_arch`
in the workflow dispatch inputs. Empty inputs select the complete suite. Filtered
runs skip the historical gate and never publish dashboard history. For example:

```bash
gh workflow run heliodor-bench.yml --ref <branch> \
  -f suite_test=test_soc_66_smp_linux_boot_4hart \
  -f suite_runner=celox -f suite_arch=aarch64
```
