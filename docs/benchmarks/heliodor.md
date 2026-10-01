# Heliodor Linux Benchmark

Heliodor is Celox's large external Veryl workload. It boots a pinned Linux image
and compares Celox's native and tiered JIT backends with synchronous and tiered
Veryl-CC using the same design revision and input workload on each architecture.
Celox tiered starts on the interpreter while native code is generated in the
background. On x86-64 it first adopts a baseline native image, then replaces it
with the optimizing image at a safe point, preserving live state and event buffers.
Code-generation tracing uses the optimizing pipeline directly.
Veryl-CC tiered starts on Cranelift and switches to C code as its
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
cache.
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

For repeated local profiling of generated native code, enable the build cache:

```bash
HELIODOR_RUNNERS=celox \
HELIODOR_CELOX_BUILD_CACHE_DIR="$PWD/target/heliodor/build-cache" \
bash scripts/run-heliodor-bench.sh run
```

The runner also accepts `--build-cache-dir DIR` directly. A hit skips frontend
analysis, optimization, and native code generation; each run initializes fresh
simulation state. `CELOX_BUILD_CACHE` logs `status=hit` or `status=miss`. Source
contents and order, project metadata and dependency mappings, test name,
optimization/pass settings, four-state mode, native memory width, SLP settings,
diagnostics, detected x86 CPU/OS capabilities, working directory relative to the
project root, and the exact
runner executable identify the compilation. Files consulted by `$readmemh`,
including absent lookup candidates, are checked by content before reuse.
Dependency namespaces and properties are resolved before cache lookup.
Component Cargo metadata, manifest candidates, and
prebuilt WASM files are also checked, including manifest modification times that
determine which interface takes precedence. Native component library presence,
including Cargo `[lib].name` overrides, is tracked so adding or removing a library
refreshes the runtime library selection. Keep build inputs stable during compilation.

Cache dependency paths and image library, file-base, and source-location paths
are stored relative to the project root. On load they are bound to the current
root. For an explicitly absolute `$readmemh` path, a hash of its physical location
also prevents a relocated file from replacing the design's fixed reference.
Moving a project preserves cache reuse when its inputs and relative
working directory stay equivalent. The key also includes each source's resolved
namespace. External paths use `..`; paths on a different Windows drive cannot be
represented and bypass caching. CLI native image exports use the same relative
paths and fail before writing if a path cannot be represented relative to the
project root. `--native-image-input` binds them using the current `--project` root
without loading source files. Existing images with absolute paths remain loadable.
Design-authored strings are preserved; the cache still contains compiled design
data and is not an anonymized artifact. Earlier cache entries are not reused by
the new relative-path format.

Caching is opt-in and supports the native backend, including host codegen in
`host-qemu` mode and `--compile-only --native-image-output`. It cannot be combined
with `--native-image-input` or `--dump-ir-dir`. Cache directories contain executable
code and must be trusted local directories. Writes are atomic; invalid entries or
cache I/O failures fall back to compilation. Delete the directory to clear it;
entries are not evicted automatically.

On a hit, `compile_ns` measures loading and initialization rather than compilation.
Use cached runs to study execution, and disable caching when comparing build or
end-to-end performance. The fixed CI `gate` always disables this cache.

The fixed CI `gate` runs `veryl-cc-sync`, `celox`, `celox-tiered`, and
`veryl-cc-tiered` on x86-64. The nightly AArch64 job measures the same four
backends. All pinned jobs use `scripts/heliodor-revision`. Each successful
suite backend publishes immediately, without waiting for other backends,
workloads, or architectures. Failed runs remain failures in CI.

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
simulated guest, not the benchmark host OS. Backends execute sequentially on the
same VM within each group below (26 jobs for the complete suite):

| Workload | Comparison groups per architecture |
| --- | --- |
| 1/2 harts, or x86-64 4 harts | All four backends in one job |
| AArch64 4 harts | Two jobs: Celox native + Veryl-CC sync; Celox tiered + Veryl-CC tiered |
| 8 harts, either architecture | Four jobs, one per backend |

Recent complete eight-hart runs total 10–13 hours on x86-64 and 17–18 hours on
AArch64. AArch64 four-hart runs total 5–7 hours, so splitting them into equivalent
execution modes preserves useful same-CPU comparisons with time for builds.
Different groups may use different CPUs. CPU and host identity are retained in
each artifact. Each successful backend publishes independently.
Each runner has a one-hour timeout for 1/2 harts,
three hours for 4 harts, and five and a half hours for 8 harts;
timeouts and incomplete runs fail the job and are not published as timings.
The suite applies testbench adjustment `8hart-100m-v1`: the 8-hart Veryl test
gets the same 100-million-cycle budget as its upstream Verilator wrapper,
replacing the stale 30-million-cycle limit. The shutdown assertion is unchanged.
`HELIODOR_SUITE=1` applies this adjustment only to the pinned suite revision;
the selected adjustment is recorded in the job log. Switching the same checkout
back to `HELIODOR_SUITE=0` restores the stock testbench. Additional local edits
to a patched wrapper are preserved and reported as an error.

Linux 7.1 SMP (2/4 harts) is excluded pending an upstream RTL fix. Both
configurations stop retiring instructions on one hart; the two-hart case also
reproduces on Verilator, with a stalled data-cache read and another hart waiting
for a lock. These failures are not counted as successful benchmark results.

The separate HEAD compatibility job skips only upstream revision
`94e9c5821c24a8941c3ddc3b76daddc7124a855a`: its testbench lacks the
`initial_assign` annotations for ROM/DRAM preloads added by `6285682`.
CI records this as a known-source exclusion, not a successful simulation.
Every other upstream HEAD remains eligible for the compatibility test.

The dashboard shows the current suite, with each kernel and hart count labeled
separately. Each chart uses the compilation, execution, and tiered timing
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
runs skip the gate. Successful results from manual runs on `master` publish
individually; runs on other branches do not publish. For example:

```bash
gh workflow run heliodor-bench.yml --ref <branch> \
  -f suite_test=test_soc_66_smp_linux_boot_4hart \
  -f suite_runner=celox -f suite_arch=aarch64
```

Nightly and manual runs use the same grouping above. To compare a subset, give
`suite_runner` a space-separated list; this narrows the groups without combining
them. Backends execute in the supplied order within each group. CPU, runner,
group, run/attempt, and boot identifiers are saved with the
results. Every Veryl-CC run still gets a fresh AOT-C cache.

```bash
gh workflow run heliodor-bench.yml --ref <branch> \
  -f suite_test=test_soc_66_smp_linux_boot_4hart \
  -f suite_runner="celox-tiered veryl-cc-tiered" -f suite_arch=aarch64
```

Each group has a shared 5-hour-50-minute budget, including building the runners.
This reserves 20 minutes beyond the longest per-runner timeout for checkout and
Rust builds, with about 10 minutes left for setup, termination, and artifact upload
before the [hosted job's six-hour limit](https://docs.github.com/en/actions/reference/limits).
A timeout or missing backend fails CI. Completed backends still publish;
incomplete boots do not publish timings. The per-backend limits above also apply within
this shared budget. Separate workflow runs, including different commits, can use different
CPUs; their history is not a same-host comparison.
